//! The core: the relay operations both front ends perform. One async call per
//! operation, no state between calls, and no rendering, wording, or exit
//! code. The TUI session and the CLI drive this module; the contracts are
//! design/shared-core.md.

use std::collections::{HashMap, HashSet};

use buzz_sdk::builders::{
    build_delete_message, build_edit, build_message, build_reaction, build_remove_reaction,
};
use buzz_sdk::{ThreadRef, extract_channel_id};
use nostr::{Event, EventBuilder, EventId, Keys, Tag};
use serde_json::{Value, json};
use uuid::Uuid;

use crate::agents;
use crate::config::Resolved;
use crate::content;
use crate::failure::{Category, Failure};
use crate::http::HttpTransport;
use crate::mentions;
use crate::read_state;

pub use crate::http::WriteOutcome;

/// The tag that names a channel id on membership and metadata events.
const D_TAG: &str = "d";
/// The tag that names a channel's display name on metadata events.
const NAME_TAG: &str = "name";
/// The tag that names a channel's type on metadata events: `stream`, `forum`,
/// `workflow`, or `dm`.
const TYPE_TAG: &str = "t";

/// The identity's own marker lookup. Bounded to the window the contract names:
/// the window is a query bound, not marker expiry.
fn read_state_filter(me: &str, now: u64) -> Value {
    json!({
        "kinds": [content::READ_STATE_KIND],
        "authors": [me],
        "#t": [READ_STATE_TAG],
        "since": now.saturating_sub(READ_STATE_WINDOW_SECS),
        "limit": READ_STATE_LIMIT,
    })
}
/// The metadata tag the relay sets on a DM, as a hint not to show it in a
/// public group list. It is not the viewer's hidden state.
const DM_HINT_TAG: &str = "hidden";
/// The metadata tag that carries the archive state.
const ARCHIVED_TAG: &str = "archived";
/// The tag that names one member on metadata events.
const P_TAG: &str = "p";
/// The tag that names one hidden channel on a visibility snapshot.
const H_TAG: &str = "h";
/// The channel type a direct conversation carries.
const DM_TYPE: &str = "dm";
/// How many channel ids one membership or metadata query carries.
const CHANNEL_QUERY_LIMIT: u64 = 500;
/// How many marker slots one answer may carry.
const READ_STATE_LIMIT: u64 = 500;
/// How far back the marker lookup reaches. The window is a query bound, not
/// marker expiry: an identity that has read nothing in a week starts from a
/// seed rather than from a frontier nobody has refreshed.
const READ_STATE_WINDOW_SECS: u64 = 7 * 24 * 60 * 60;
/// The second tag every read-state slot carries.
const READ_STATE_TAG: &str = "read-state";
/// How many events one conversation's catch-up asks for. A full page is a
/// truncated answer, not a complete one.
const CATCH_UP_LIMIT: u64 = 1000;
/// How many filters one catch-up REQ carries: the relay's per-request ceiling.
const CHUNK_FILTERS: usize = 10;
/// How many replies one thread read carries.
const THREAD_LIMIT: u64 = 500;
/// How many authors one profile query carries.
const PROFILE_CHUNK: usize = 50;
/// How many authors one profile read resolves. Beyond this, a name stays a
/// short key.
const PROFILE_LIMIT: usize = 200;

/// What the relay's channel metadata says a membership row is. A row the
/// relay never described cannot be classified: the tags it lacks say nothing.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ChannelKind {
    /// A channel: a stream, a forum, or a workflow channel.
    Channel,
    /// A direct conversation. Its label is its participants.
    Dm,
    /// No metadata for this row, so neither its kind nor its archive state is
    /// known.
    Unknown,
}

/// One membership row as the relay describes it. The metadata is the only
/// place that says what a row is, who is in a DM, and whether it is archived.
#[derive(Debug, Clone, PartialEq)]
pub struct ChannelInfo {
    pub id: Uuid,
    /// The metadata name, or the id when the row has none.
    pub name: String,
    pub kind: ChannelKind,
    /// A DM's other members, in metadata order. Empty for a channel.
    pub participants: Vec<String>,
    pub archived: bool,
    /// The viewer hid this DM from its list, per the relay's snapshot.
    pub hidden: bool,
}

/// The membership roster and how much of it could be classified. `complete`
/// is false when a query the classification depends on failed, or when a row
/// arrived without metadata: an empty hidden set and a nameless row are then
/// not proof of anything, so callers report incomplete coverage instead of
/// showing the list as authoritative.
#[derive(Debug, Clone)]
pub struct Roster {
    pub items: Vec<ChannelInfo>,
    pub complete: bool,
}

/// What one conversation's catch-up asks for.
#[derive(Debug, Clone, Copy)]
pub enum CatchUp {
    /// Everything at or after the read frontier.
    Since { channel: Uuid, since: u64 },
    /// The newest message, for a conversation with no marker yet: the baseline
    /// a local track starts from. An empty answer is the whole answer.
    Newest { channel: Uuid },
}

impl CatchUp {
    pub fn channel(&self) -> Uuid {
        match self {
            CatchUp::Since { channel, .. } | CatchUp::Newest { channel } => *channel,
        }
    }

    /// One filter per conversation: each carries a frontier of its own, and
    /// `#h` names one channel.
    fn filter(&self) -> Value {
        match self {
            CatchUp::Since { channel, since } => json!({
                "kinds": content::TIMELINE_KINDS,
                "#h": [channel.to_string()],
                "limit": CATCH_UP_LIMIT,
                "since": since,
            }),
            CatchUp::Newest { channel } => json!({
                "kinds": content::TIMELINE_KINDS,
                "#h": [channel.to_string()],
                "limit": 1,
            }),
        }
    }
}

/// One conversation's catch-up answer.
#[derive(Debug, Clone)]
pub struct CatchUpAnswer {
    pub channel: Uuid,
    /// What the query returned, oldest first. A `Newest` answer holds at most
    /// one event.
    pub events: Vec<Event>,
    /// True for a `Newest` request, so the caller can tell a baseline from a
    /// catch-up.
    pub newest: bool,
    /// False when the query failed, or when it returned a full page: the
    /// rest may simply not have been asked for.
    pub complete: bool,
}

/// Sort one batch's answer into one answer per request. A batch the relay did
/// not answer is incomplete for every conversation in it; a full page leaves
/// the rest unasked for, which is also incomplete. Grouping is by the channel
/// tag, because one query can name several conversations.
fn answers_from(requests: &[CatchUp], found: Result<Vec<Event>, Failure>) -> Vec<CatchUpAnswer> {
    let (found, answered) = match found {
        Ok(events) => (events, true),
        Err(_) => (Vec::new(), false),
    };
    requests
        .iter()
        .map(|request| {
            let channel = request.channel();
            let events: Vec<Event> = found
                .iter()
                .filter(|event| channel_of(event) == Some(channel))
                .cloned()
                .collect();
            let events = order_timeline(events);
            // A full page means more may exist. A newest-message answer is the
            // whole question by construction.
            let truncated =
                matches!(request, CatchUp::Since { .. }) && events.len() as u64 >= CATCH_UP_LIMIT;
            CatchUpAnswer {
                channel,
                newest: matches!(request, CatchUp::Newest { .. }),
                events,
                complete: answered && !truncated,
            }
        })
        .collect()
}

/// A membership row is not automatically a conversation a list shows: the
/// viewer's own hidden DMs and archived channels stay out of the list.
impl ChannelInfo {
    pub fn listed(&self) -> bool {
        !self.hidden && !self.archived
    }
}

/// Where a reply to one event goes: its channel, and the NIP-10 thread
/// context of the event being answered.
pub struct Routing {
    pub channel: Uuid,
    pub thread: ThreadRef,
}

/// One bounded thread read: the root event first, then its replies, oldest
/// first, and whether the reply query was saturated.
pub struct ThreadRead {
    pub events: Vec<Event>,
    /// The reply query answered with as many events as it was allowed to
    /// return. The thread may hold more than this read carries, so a caller
    /// must not present the result as complete history.
    pub partial: bool,
}

/// Parse a channel UUID from user input.
pub fn channel_id(raw: &str) -> Result<Uuid, Failure> {
    Uuid::parse_str(raw)
        .map_err(|_| Failure::invalid_input(format!("channel must be a UUID: {raw}")))
}

/// Parse an event id from user input.
pub fn event_id(raw: &str) -> Result<EventId, Failure> {
    EventId::from_hex(raw)
        .map_err(|_| Failure::invalid_input(format!("event id must be 64 hex characters: {raw}")))
}

/// The channel a reply goes to, and the thread context that puts it in the
/// right place. A reply answers its target; the thread root is the target's
/// own root, or the target itself when it starts the thread. `ThreadRef`
/// collapses root and parent into one `e` tag, so a direct reply and a
/// nested reply come out of the same rule.
pub fn routing(target: &Event) -> Result<Routing, Failure> {
    let channel = extract_channel_id(target).ok_or_else(|| {
        Failure::invalid_input(format!(
            "event {} is not channel-scoped",
            target.id.to_hex()
        ))
    })?;
    // A malformed marker on a stored event is treated as no marker: the
    // target starts its own thread.
    let root = content::root_of(target)
        .and_then(|hex| EventId::from_hex(&hex).ok())
        .unwrap_or(target.id);
    Ok(Routing {
        channel,
        thread: ThreadRef {
            root_event_id: root,
            parent_event_id: target.id,
        },
    })
}

/// The relay operations both front ends perform.
pub struct Client {
    transport: HttpTransport,
    keys: Keys,
    auth_tag: Option<Tag>,
}

impl Client {
    /// Build the client for one identity and relay. `Resolved` has already
    /// verified the auth tag, so a tag that does not parse here is a bug.
    pub fn new(resolved: &Resolved) -> Client {
        let auth_tag = resolved.auth_tag.as_deref().map(|tag| {
            buzz_sdk::nip_oa::parse_auth_tag(tag).expect("resolve() verified the tag at startup")
        });
        Client {
            transport: HttpTransport::new(
                &resolved.http_url,
                resolved.keys.clone(),
                resolved.auth_tag.clone(),
            ),
            keys: resolved.keys.clone(),
            auth_tag,
        }
    }

    /// The channels the identity belongs to: the membership roster, the
    /// metadata that describes it, and the viewer's hidden-DM snapshot. The
    /// roster itself must load; the two queries that classify it degrade to an
    /// incomplete roster, never to an empty list and never to an authoritative
    /// "nothing is hidden".
    pub async fn channels(&self) -> Result<Roster, Failure> {
        let me = self.keys.public_key().to_hex();
        let membership = self
            .transport
            .query(&json!({
                "kinds": [content::MEMBERSHIP_KIND],
                "#p": [me],
                "limit": CHANNEL_QUERY_LIMIT,
            }))
            .await?;
        let ids = roster_ids(&membership);
        if ids.is_empty() {
            return Ok(Roster {
                items: Vec::new(),
                complete: true,
            });
        }
        let described = self
            .transport
            .query(&json!({
                "kinds": [content::CHANNEL_METADATA_KIND],
                "#d": ids,
                "limit": CHANNEL_QUERY_LIMIT,
            }))
            .await;
        let hidden = self.hidden_dms().await;
        let none = HashSet::new();
        let items = channels_from(
            &membership,
            described.as_deref().unwrap_or_default(),
            hidden.as_ref().unwrap_or(&none),
            &me,
        );
        let complete = described.is_ok()
            && hidden.is_ok()
            && items.iter().all(|item| item.kind != ChannelKind::Unknown);
        Ok(Roster { items, complete })
    }

    /// The managed-agent records this identity owns, read as a roster. Ownership
    /// is the record's author: only the owner's own key signs one, so the query
    /// is scoped to it and `from_events` re-checks the author before claiming
    /// anything. A read that fails is an error, never an empty roster.
    pub async fn managed_agents(&self) -> Result<agents::Roster, Failure> {
        let me = self.keys.public_key().to_hex();
        let events = self
            .transport
            .query(&json!({
                "kinds": [agents::KIND_MANAGED_AGENT],
                "authors": [me],
                "limit": agents::ROSTER_LIMIT,
            }))
            .await?;
        Ok(agents::Roster::from_events(&events, &me))
    }

    /// The channels this identity hid from its direct list, from the relay's
    /// replaceable visibility snapshot. A snapshot it never received means it
    /// never hid one, which is an answer, not a gap.
    async fn hidden_dms(&self) -> Result<HashSet<String>, Failure> {
        let me = self.keys.public_key().to_hex();
        let snapshot = self
            .transport
            .query(&json!({
                "kinds": [content::DM_VISIBILITY_KIND],
                "#p": [me],
                "limit": 1,
            }))
            .await?;
        Ok(hidden_ids(&snapshot))
    }

    /// The identity's read state: every marker slot it owns, merged. A slot
    /// that cannot be decoded is a gap, and the caller reports incomplete
    /// coverage rather than reading absence into it.
    pub async fn read_state(&self) -> Result<read_state::ReadState, Failure> {
        let me = self.keys.public_key().to_hex();
        let events = self
            .transport
            .query(&read_state_filter(&me, crate::sub::now_secs()))
            .await?;
        Ok(read_state::parse(&self.keys, &events))
    }

    /// Read the remote state back, merge this terminal's picture into it, and
    /// publish one marker carrying the result. The read-back is what keeps a
    /// second terminal's progress from being overwritten, so a failed read is
    /// a failed publish rather than a blind write. On success the merged
    /// contexts come back, which is a fresh answer for the caller as well.
    pub async fn publish_read_state(
        &self,
        contexts: &HashMap<String, u64>,
    ) -> Result<HashMap<String, u64>, String> {
        let remote = self
            .read_state()
            .await
            .map_err(|failure| format!("read-back failed: {failure}"))?;
        if remote.gaps {
            return Err("a read-state slot could not be decoded".to_owned());
        }
        let mut merged = remote.contexts;
        for (key, at) in contexts {
            let entry = merged.entry(key.clone()).or_insert(0);
            *entry = (*entry).max(*at);
        }
        let builder = read_state::builder(&self.keys, &merged)?;
        match self.submit(builder).await {
            WriteOutcome::Stored { .. } => Ok(merged),
            WriteOutcome::Refused { reason, .. } | WriteOutcome::Unknown { reason, .. } => {
                Err(reason)
            }
        }
    }

    /// Catch up a batch of conversations: what each holds at or after its own
    /// frontier, or its newest message when it has none. The relay accepts a
    /// bounded number of filters in one REQ, so the batch is chunked here, and
    /// the answers are attributed by the channel tag the events carry.
    pub async fn catch_up(&self, requests: &[CatchUp]) -> Vec<CatchUpAnswer> {
        let mut answers: Vec<CatchUpAnswer> = Vec::with_capacity(requests.len());
        for chunk in requests.chunks(CHUNK_FILTERS) {
            let filters: Vec<Value> = chunk.iter().map(|request| request.filter()).collect();
            let found = self.transport.query_all(&filters).await;
            answers.extend(answers_from(chunk, found));
        }
        answers
    }

    /// One channel's timeline, oldest first.
    pub async fn history(&self, channel: Uuid, limit: u64) -> Result<Vec<Event>, Failure> {
        let events = self
            .transport
            .query(&json!({
                "kinds": content::TIMELINE_KINDS,
                "#h": [channel.to_string()],
                "limit": limit,
            }))
            .await?;
        Ok(order_timeline(events))
    }

    /// A thread: the root event first, then its replies, oldest first.
    ///
    /// `channel` is the conversation the caller is in. When it is given, the
    /// replies are read from that conversation only (`#h`), the root must
    /// belong to it, and a reply counts only when its own existing root
    /// semantics resolve to the read's root - an `e` reference alone does not
    /// make an event part of the thread. A root that sits in another
    /// conversation is refused rather than followed. The CLI prints a thread
    /// with no conversation in hand and passes `None`.
    pub async fn thread(
        &self,
        root: EventId,
        channel: Option<Uuid>,
    ) -> Result<ThreadRead, Failure> {
        let hex = root.to_hex();
        let mut found = self
            .transport
            .query(&json!({ "ids": [hex], "limit": 1 }))
            .await?;
        let Some(root_event) = found.pop() else {
            return Err(Failure::not_found(format!("no event {hex}")));
        };
        if let Some(channel) = channel {
            match extract_channel_id(&root_event) {
                Some(in_channel) if in_channel == channel => {}
                Some(_) => {
                    return Err(Failure::invalid_input(format!(
                        "thread root {hex} is in another conversation"
                    )));
                }
                None => {
                    return Err(Failure::invalid_input(format!(
                        "thread root {hex} is not channel-scoped"
                    )));
                }
            }
        }
        let mut filter = json!({
            "kinds": content::TIMELINE_KINDS,
            "#e": [hex],
            "limit": THREAD_LIMIT,
        });
        if let Some(channel) = channel {
            filter["#h"] = json!([channel.to_string()]);
        }
        let replies = self.transport.query(&filter).await?;
        let (replies, partial) = thread_replies(replies, &hex, channel);
        Ok(ThreadRead {
            events: thread_events(&root_event, replies),
            partial,
        })
    }

    /// One event by id.
    pub async fn event(&self, id: EventId) -> Result<Event, Failure> {
        let hex = id.to_hex();
        let mut found = self
            .transport
            .query(&json!({ "ids": [hex], "limit": 1 }))
            .await?;
        found
            .pop()
            .ok_or_else(|| Failure::not_found(format!("no event {hex}")))
    }

    /// Display names for the authors a view has seen. A failed chunk fails
    /// the read; a caller that can live with short keys ignores it.
    pub async fn profiles(&self, pubkeys: Vec<String>) -> Result<Vec<(String, String)>, Failure> {
        let mut seen: HashSet<String> = HashSet::new();
        let wanted: Vec<String> = pubkeys
            .into_iter()
            .filter(|key| seen.insert(key.clone()))
            .take(PROFILE_LIMIT)
            .collect();
        let mut resolved: Vec<(String, String)> = Vec::new();
        for chunk in wanted.chunks(PROFILE_CHUNK) {
            let found = self
                .transport
                .query(&json!({
                    "kinds": [content::PROFILE_KIND],
                    "authors": chunk,
                    "limit": chunk.len() as u64,
                }))
                .await?;
            for event in found {
                if let Some((name, display)) = content::profile_names(&event)
                    && let Some(best) = display.or(name)
                {
                    resolved.push((event.pubkey.to_hex(), best));
                }
            }
        }
        Ok(resolved)
    }

    /// The current membership and the member profiles a mention preflight
    /// matches names against. A failed read is an error, never an empty
    /// directory: an incomplete roster would drop the recipients a draft
    /// names without saying so.
    pub async fn mention_directory(&self, channel: Uuid) -> Result<mentions::Directory, Failure> {
        let rosters = self
            .transport
            .query(&json!({
                "kinds": [content::MEMBERSHIP_KIND],
                "#d": [channel.to_string()],
                "limit": 1,
            }))
            .await?;
        let members = rosters
            .first()
            .map(content::member_pubkeys)
            .unwrap_or_default();
        let mut profiles: Vec<(String, String)> = Vec::new();
        for chunk in members.chunks(PROFILE_CHUNK) {
            let found = self
                .transport
                .query(&json!({
                    "kinds": [content::PROFILE_KIND],
                    "authors": chunk,
                    "limit": chunk.len() as u64,
                }))
                .await?;
            profiles.extend(
                found
                    .into_iter()
                    .map(|event| (event.pubkey.to_hex(), event.content.clone())),
            );
        }
        Ok(mentions::Directory { members, profiles })
    }

    /// Attach the identity's NIP-OA tag, sign, and submit. Every write the
    /// two front ends perform goes through here.
    pub async fn submit(&self, builder: EventBuilder) -> WriteOutcome {
        let builder = match &self.auth_tag {
            Some(tag) => builder.tag(tag.clone()),
            None => builder,
        };
        let event = match builder.sign_with_keys(&self.keys) {
            Ok(event) => event,
            Err(e) => return refused(Category::InvalidInput, format!("signing failed: {e}")),
        };
        self.transport.submit(&event).await
    }

    /// A top-level message, or a reply when `thread` is given. `mentions` are
    /// the recipients the caller resolved from the content; the builder turns
    /// them into the signed `p` tags and applies its own cap.
    pub async fn send_message(
        &self,
        channel: Uuid,
        content: &str,
        thread: Option<ThreadRef>,
        mentions: &[String],
    ) -> WriteOutcome {
        if content.is_empty() {
            return refused(Category::InvalidInput, "content is empty".to_owned());
        }
        let mentions: Vec<&str> = mentions.iter().map(String::as_str).collect();
        match build_message(channel, content, thread.as_ref(), &mentions, false, &[]) {
            Ok(builder) => self.submit(builder).await,
            Err(e) => refused(Category::InvalidInput, e.to_string()),
        }
    }

    /// Answer one event: its channel and its thread context, derived from the
    /// event itself. The CLI's reply has no mention input of its own, so it
    /// carries no recipients; a TUI reply goes through `send_message` with the
    /// thread the composer holds.
    pub async fn reply(&self, target: &Event, content: &str) -> WriteOutcome {
        let route = match routing(target) {
            Ok(route) => route,
            Err(failure) => return failure.into(),
        };
        self.send_message(route.channel, content, Some(route.thread), &[])
            .await
    }

    /// Replace the content of the identity's own message.
    pub async fn edit(&self, channel: Uuid, target: EventId, content: &str) -> WriteOutcome {
        if content.is_empty() {
            return refused(Category::InvalidInput, "content is empty".to_owned());
        }
        match build_edit(channel, target, content) {
            Ok(builder) => self.submit(builder).await,
            Err(e) => refused(Category::InvalidInput, e.to_string()),
        }
    }

    /// Delete the identity's own message.
    pub async fn delete(&self, channel: Uuid, target: EventId) -> WriteOutcome {
        match build_delete_message(channel, target) {
            Ok(builder) => self.submit(builder).await,
            Err(e) => refused(Category::InvalidInput, e.to_string()),
        }
    }

    /// React to one event.
    pub async fn react(&self, target: EventId, emoji: &str) -> WriteOutcome {
        match build_reaction(target, emoji) {
            Ok(builder) => self.submit(builder).await,
            Err(e) => refused(Category::InvalidInput, e.to_string()),
        }
    }

    /// Remove one of the identity's own reactions, named by its event id.
    pub async fn remove_reaction(&self, reaction: EventId) -> WriteOutcome {
        match build_remove_reaction(reaction) {
            Ok(builder) => self.submit(builder).await,
            Err(e) => refused(Category::InvalidInput, e.to_string()),
        }
    }
}

/// A builder the SDK refused is bad input: nothing was signed, nothing was
/// sent, and the caller may fix the arguments.
fn refused(category: Category, reason: String) -> WriteOutcome {
    WriteOutcome::Refused { category, reason }
}

fn first_tag(event: &Event, name: &str) -> Option<String> {
    event.tags.iter().find_map(|tag| named_tag(tag, name))
}

/// The value of a `[name, value]` tag, when the tag carries one.
fn named_tag(tag: &Tag, name: &str) -> Option<String> {
    let parts = tag.as_slice();
    (parts.first().map(String::as_str) == Some(name))
        .then(|| parts.get(1).cloned())
        .flatten()
}

/// Whether an event carries a tag by that name, with or without a value.
fn has_tag(event: &Event, name: &str) -> bool {
    event
        .tags
        .iter()
        .any(|tag| tag.as_slice().first().map(String::as_str) == Some(name))
}

/// The channel an event is scoped to, from its `h` tag. An event without a
/// usable one cannot be placed in a conversation.
fn channel_of(event: &Event) -> Option<Uuid> {
    event
        .tags
        .iter()
        .find_map(|tag| named_tag(tag, H_TAG))
        .and_then(|id| Uuid::parse_str(&id).ok())
}

/// The channel ids a membership roster names, in the order it names them.
fn roster_ids(roster: &[Event]) -> Vec<String> {
    let mut seen: HashSet<String> = HashSet::new();
    roster
        .iter()
        .filter_map(|event| first_tag(event, D_TAG))
        .filter(|id| !id.is_empty() && seen.insert(id.clone()))
        .collect()
}

/// The channel ids the newest visibility snapshot marks hidden. The snapshot
/// replaces its predecessor, so it is the whole set: an id it does not name is
/// not hidden. Ties on time are broken by event id so one order holds.
fn hidden_ids(snapshot: &[Event]) -> HashSet<String> {
    let Some(newest) = snapshot
        .iter()
        .max_by_key(|event| (event.created_at, event.id.to_hex()))
    else {
        return HashSet::new();
    };
    newest
        .tags
        .iter()
        .filter_map(|tag| named_tag(tag, H_TAG))
        .filter(|id| !id.is_empty())
        .collect()
}

/// The part of a metadata event that classifies its row.
struct Metadata {
    name: Option<String>,
    kind: ChannelKind,
    participants: Vec<String>,
    archived: bool,
}

/// One entry per roster id, classified by the metadata that describes it. A
/// row the relay did not describe is `Unknown`, not a channel, and a row the
/// viewer hid is marked hidden whatever its kind.
fn channels_from(
    roster: &[Event],
    metadata: &[Event],
    hidden: &HashSet<String>,
    me: &str,
) -> Vec<ChannelInfo> {
    let mut described: HashMap<String, Metadata> = HashMap::new();
    for event in metadata {
        let Some(id) = first_tag(event, D_TAG) else {
            continue;
        };
        // The type tag is authoritative; a DM also carries the relay's hidden
        // hint, which classifies a row that predates the type tag.
        let kind = match first_tag(event, TYPE_TAG).as_deref() {
            Some(DM_TYPE) => ChannelKind::Dm,
            Some(_) => ChannelKind::Channel,
            None if has_tag(event, DM_HINT_TAG) => ChannelKind::Dm,
            None => ChannelKind::Channel,
        };
        let participants = match kind {
            // Only a DM's metadata lists its members: on a channel the same
            // tag names its admins.
            ChannelKind::Dm => event
                .tags
                .iter()
                .filter_map(|tag| named_tag(tag, P_TAG))
                .filter(|pubkey| pubkey != me)
                .collect(),
            _ => Vec::new(),
        };
        described.entry(id).or_insert(Metadata {
            name: first_tag(event, NAME_TAG).filter(|name| !name.is_empty()),
            kind,
            participants,
            archived: first_tag(event, ARCHIVED_TAG).as_deref() == Some("true"),
        });
    }
    let mut channels: Vec<ChannelInfo> = roster_ids(roster)
        .into_iter()
        .filter_map(|id| Uuid::parse_str(&id).ok().map(|uuid| (id, uuid)))
        .map(|(id, uuid)| {
            let info = described.get(&id);
            ChannelInfo {
                id: uuid,
                name: info
                    .and_then(|info| info.name.clone())
                    .unwrap_or_else(|| id.clone()),
                kind: info.map_or(ChannelKind::Unknown, |info| info.kind),
                participants: info
                    .map(|info| info.participants.clone())
                    .unwrap_or_default(),
                archived: info.is_some_and(|info| info.archived),
                hidden: hidden.contains(&id),
            }
        })
        .collect();
    // A DM's own name is the literal "DM", so its section is ordered by id:
    // stable, and a label that resolves later never reorders the list.
    channels.sort_by(|a, b| {
        a.name
            .to_lowercase()
            .cmp(&b.name.to_lowercase())
            .then(a.id.cmp(&b.id))
    });
    channels
}

/// The relay returns newest first; both front ends render oldest first, and
/// two events in the same second keep one order.
fn order_timeline(mut events: Vec<Event>) -> Vec<Event> {
    events.sort_by(|a, b| {
        a.created_at
            .as_secs()
            .cmp(&b.created_at.as_secs())
            .then(a.id.to_hex().cmp(&b.id.to_hex()))
    });
    events
}

/// The replies of one thread out of a raw `#e` answer, and whether that answer
/// was saturated. Saturation is decided on the raw answer - before
/// deduplication and before the root check drops anything - because a full
/// page of `#e` references is a possibly-partial thread, not a complete one.
fn thread_replies(raw: Vec<Event>, root: &str, channel: Option<Uuid>) -> (Vec<Event>, bool) {
    let partial = raw.len() as u64 >= THREAD_LIMIT;
    let replies = raw
        .into_iter()
        .filter(|event| {
            content::root_of(event).as_deref() == Some(root)
                && match channel {
                    Some(channel) => extract_channel_id(event) == Some(channel),
                    None => true,
                }
        })
        .collect();
    (replies, partial)
}

/// The root first, then its replies, deduplicated by event id.
fn thread_events(root: &Event, replies: Vec<Event>) -> Vec<Event> {
    let mut seen: HashSet<String> = HashSet::new();
    seen.insert(root.id.to_hex());
    let replies: Vec<Event> = replies
        .into_iter()
        .filter(|event| seen.insert(event.id.to_hex()))
        .collect();
    let mut ordered = Vec::with_capacity(replies.len() + 1);
    ordered.push(root.clone());
    ordered.extend(order_timeline(replies));
    ordered
}

#[cfg(test)]
mod tests {
    use super::*;
    use nostr::Kind;

    fn keys() -> Keys {
        Keys::generate()
    }

    fn signed(keys: &Keys, tags: Vec<Tag>) -> Event {
        EventBuilder::new(Kind::Custom(9), "hello")
            .tags(tags)
            .sign_with_keys(keys)
            .expect("signing a test event")
    }

    fn at(keys: &Keys, seconds: u64) -> Event {
        EventBuilder::new(Kind::Custom(9), "hello")
            .custom_created_at(nostr::Timestamp::from_secs(seconds))
            .sign_with_keys(keys)
            .expect("signing a test event")
    }

    fn channel() -> Uuid {
        Uuid::from_u128(0x7e5f_aaba_948a_47b5_8ca0_20c6_e479_53d3)
    }

    fn h_tag() -> Tag {
        Tag::parse(["h", &channel().to_string()]).unwrap()
    }

    fn e_tag(id: &EventId, marker: &str) -> Tag {
        Tag::parse(["e", &id.to_hex(), "", marker]).unwrap()
    }

    fn message(keys: &Keys, channel: Uuid, body: &str, at: u64) -> Event {
        EventBuilder::new(Kind::Custom(9), body)
            .tags(vec![Tag::parse(["h", &channel.to_string()]).unwrap()])
            .custom_created_at(nostr::Timestamp::from_secs(at))
            .sign_with_keys(keys)
            .expect("signing a test message")
    }

    fn metadata(
        keys: &Keys,
        id: u128,
        kind: Option<&str>,
        name: Option<&str>,
        participants: &[String],
        archived: bool,
    ) -> Event {
        let mut tags = vec![Tag::parse(["d", &Uuid::from_u128(id).to_string()]).unwrap()];
        if let Some(kind) = kind {
            tags.push(Tag::parse(["t", kind]).unwrap());
        }
        if let Some(name) = name {
            tags.push(Tag::parse(["name", name]).unwrap());
        }
        for pubkey in participants {
            tags.push(Tag::parse(["p", pubkey]).unwrap());
        }
        if archived {
            tags.push(Tag::parse(["archived", "true"]).unwrap());
        }
        EventBuilder::new(Kind::Custom(content::CHANNEL_METADATA_KIND as u16), "")
            .tags(tags)
            .sign_with_keys(keys)
            .expect("signing test metadata")
    }

    fn ids(events: &[Event]) -> Vec<String> {
        events.iter().map(|event| event.id.to_hex()).collect()
    }

    #[test]
    fn catch_up_answers_group_a_batch_by_conversation_in_timeline_order() {
        let keys = keys();
        let one = Uuid::from_u128(1);
        let two = Uuid::from_u128(2);
        let elsewhere = Uuid::from_u128(3);
        let late = message(&keys, one, "late", 30);
        let early = message(&keys, one, "early", 10);
        let only = message(&keys, two, "only", 20);
        let answers = answers_from(
            &[
                CatchUp::Since {
                    channel: one,
                    since: 5,
                },
                CatchUp::Newest { channel: two },
            ],
            Ok(vec![
                late.clone(),
                message(&keys, elsewhere, "other", 15),
                early.clone(),
                only.clone(),
            ]),
        );
        assert_eq!(answers.len(), 2);
        assert_eq!(answers[0].channel, one);
        assert_eq!(
            ids(&answers[0].events),
            vec![early.id.to_hex(), late.id.to_hex()],
            "a conversation's answer is oldest first"
        );
        assert!(!answers[0].newest);
        assert!(answers[0].complete);
        assert_eq!(answers[1].channel, two);
        assert!(answers[1].newest);
        assert_eq!(ids(&answers[1].events), vec![only.id.to_hex()]);
    }

    #[test]
    fn a_batch_the_relay_did_not_answer_is_incomplete_for_every_conversation() {
        let answers = answers_from(
            &[
                CatchUp::Since {
                    channel: Uuid::from_u128(1),
                    since: 0,
                },
                CatchUp::Newest {
                    channel: Uuid::from_u128(2),
                },
            ],
            Err(Failure::network("down")),
        );
        assert!(answers.iter().all(|answer| answer.events.is_empty()));
        assert!(
            answers.iter().all(|answer| !answer.complete),
            "a failed batch said nothing about any conversation in it"
        );
    }

    #[test]
    fn a_full_page_of_a_since_answer_is_incomplete_and_a_newest_answer_is_whole() {
        let keys = keys();
        let one = Uuid::from_u128(1);
        let full: Vec<Event> = (0..CATCH_UP_LIMIT)
            .map(|n| message(&keys, one, "page", n))
            .collect();
        let answers = answers_from(
            &[
                CatchUp::Since {
                    channel: one,
                    since: 0,
                },
                CatchUp::Newest { channel: one },
            ],
            Ok(full),
        );
        assert!(
            !answers[0].complete,
            "a full page may have more behind it, so it is not an answer"
        );
        assert!(
            answers[1].complete,
            "the newest message answers the whole question by construction"
        );
    }

    #[test]
    fn a_since_filter_carries_the_frontier_and_a_newest_filter_asks_for_one() {
        let filter = CatchUp::Since {
            channel: channel(),
            since: 42,
        }
        .filter();
        assert_eq!(filter["since"], 42);
        assert_eq!(filter["limit"], CATCH_UP_LIMIT);
        assert_eq!(filter["#h"][0], channel().to_string());

        let filter = CatchUp::Newest { channel: channel() }.filter();
        assert_eq!(filter["limit"], 1);
        assert!(filter.get("since").is_none(), "a baseline is not a window");
    }

    #[test]
    fn the_marker_lookup_is_bounded_to_the_window_the_contract_names() {
        let now = 1_000_000_000;
        let filter = read_state_filter("me", now);
        assert_eq!(filter["since"], now - READ_STATE_WINDOW_SECS);
        assert_eq!(filter["limit"], READ_STATE_LIMIT);
        assert_eq!(filter["#t"], json!([READ_STATE_TAG]));
        assert_eq!(filter["authors"], json!(["me"]));
        assert_eq!(
            filter["since"],
            now - 7 * 24 * 60 * 60,
            "seven days, as the decision states"
        );

        // A clock near the epoch cannot ask for a window that starts before it.
        assert_eq!(read_state_filter("me", 10)["since"], 0);
    }

    #[test]
    fn a_row_is_classified_by_its_metadata_and_a_hidden_dm_stays_out_of_the_list() {
        let keys = keys();
        let me = keys.public_key().to_hex();
        let other = Keys::generate().public_key().to_hex();
        let (named, dm, archived) = (Uuid::from_u128(1), Uuid::from_u128(2), Uuid::from_u128(3));
        let roster = vec![
            signed(&keys, vec![Tag::parse(["d", &named.to_string()]).unwrap()]),
            signed(&keys, vec![Tag::parse(["d", &dm.to_string()]).unwrap()]),
            signed(
                &keys,
                vec![Tag::parse(["d", &archived.to_string()]).unwrap()],
            ),
        ];
        let metadata = vec![
            metadata(&keys, 1, None, Some("general"), &[], false),
            // A DM is named by its participants, and the viewer is not one of
            // the names it shows.
            metadata(
                &keys,
                2,
                Some(DM_TYPE),
                None,
                &[other.clone(), me.clone()],
                false,
            ),
            metadata(&keys, 3, None, Some("old"), &[], true),
        ];
        let items = channels_from(&roster, &metadata, &HashSet::new(), &me);
        assert_eq!(items.len(), 3);
        assert_eq!(
            items.iter().find(|item| item.id == named).unwrap().kind,
            ChannelKind::Channel
        );
        let dm_item = items.iter().find(|item| item.id == dm).unwrap();
        assert_eq!(dm_item.kind, ChannelKind::Dm);
        assert_eq!(dm_item.participants, vec![other.clone()]);
        assert!(
            items
                .iter()
                .find(|item| item.id == archived)
                .unwrap()
                .archived
        );
        assert_eq!(
            items.iter().filter(|item| item.listed()).count(),
            2,
            "an archived channel is not the list either"
        );
        assert!(items.iter().all(|item| !item.hidden));

        // The viewer's own hidden snapshot takes a DM out of the list without
        // claiming the row is not a DM.
        let hidden: HashSet<String> = HashSet::from([dm.to_string()]);
        let items = channels_from(&roster, &metadata, &hidden, &me);
        let dm_item = items.iter().find(|item| item.id == dm).unwrap();
        assert!(dm_item.hidden);
        assert!(!dm_item.listed());
        assert_eq!(
            items.iter().filter(|item| item.listed()).count(),
            1,
            "a hidden DM and an archived channel are not the list"
        );
    }

    #[test]
    fn a_row_the_relay_did_not_describe_is_unknown_not_a_channel() {
        let keys = keys();
        let me = keys.public_key().to_hex();
        let bare = Uuid::from_u128(9);
        let roster = vec![signed(
            &keys,
            vec![Tag::parse(["d", &bare.to_string()]).unwrap()],
        )];
        let items = channels_from(&roster, &[], &HashSet::new(), &me);
        assert_eq!(items.len(), 1);
        assert_eq!(items[0].kind, ChannelKind::Unknown);
        assert_eq!(
            items[0].name,
            bare.to_string(),
            "the id stands in for a name"
        );
    }

    #[test]
    fn the_hidden_snapshot_answers_with_its_newest_event() {
        let keys = keys();
        let one = Uuid::from_u128(1);
        let two = Uuid::from_u128(2);
        let older = EventBuilder::new(Kind::Custom(content::DM_VISIBILITY_KIND as u16), "")
            .tags(vec![Tag::parse(["h", &one.to_string()]).unwrap()])
            .custom_created_at(nostr::Timestamp::from_secs(10))
            .sign_with_keys(&keys)
            .unwrap();
        let newer = EventBuilder::new(Kind::Custom(content::DM_VISIBILITY_KIND as u16), "")
            .tags(vec![Tag::parse(["h", &two.to_string()]).unwrap()])
            .custom_created_at(nostr::Timestamp::from_secs(20))
            .sign_with_keys(&keys)
            .unwrap();
        let hidden = hidden_ids(&[older, newer]);
        assert_eq!(hidden, HashSet::from([two.to_string()]));
        assert!(
            hidden_ids(&[]).is_empty(),
            "a snapshot that never arrived means nothing was hidden"
        );
    }

    #[test]
    fn routing_answers_a_top_level_message_in_its_own_thread() {
        let keys = keys();
        let target = signed(&keys, vec![h_tag()]);
        let route = routing(&target).unwrap();
        assert_eq!(route.channel, channel());
        assert_eq!(route.thread.root_event_id, target.id);
        assert_eq!(route.thread.parent_event_id, target.id);
    }

    #[test]
    fn routing_follows_a_reply_back_to_its_thread_root() {
        let keys = keys();
        let root = signed(&keys, vec![h_tag()]);
        let direct = signed(&keys, vec![h_tag(), e_tag(&root.id, "reply")]);
        let nested = signed(
            &keys,
            vec![h_tag(), e_tag(&root.id, "root"), e_tag(&direct.id, "reply")],
        );
        // Answering the root starts a direct reply: root and parent are it.
        let answer_root = routing(&root).unwrap();
        assert_eq!(answer_root.thread.root_event_id, root.id);
        assert_eq!(answer_root.thread.parent_event_id, root.id);
        // Answering a direct reply keeps the thread and names it as parent.
        let answer_direct = routing(&direct).unwrap();
        assert_eq!(answer_direct.thread.root_event_id, root.id);
        assert_eq!(answer_direct.thread.parent_event_id, direct.id);
        // Answering a nested reply does the same, one level deeper.
        let answer_nested = routing(&nested).unwrap();
        assert_eq!(answer_nested.thread.root_event_id, root.id);
        assert_eq!(answer_nested.thread.parent_event_id, nested.id);
    }

    #[test]
    fn routing_refuses_an_event_that_is_not_channel_scoped() {
        let keys = keys();
        let target = signed(&keys, Vec::new());
        let Err(failure) = routing(&target) else {
            panic!("an event without a channel cannot route a reply");
        };
        assert_eq!(failure.category, Category::InvalidInput);
    }

    #[test]
    fn channels_keep_every_roster_id_and_borrow_the_metadata_name() {
        let keys = keys();
        let named = Uuid::from_u128(1);
        let unnamed = Uuid::from_u128(2);
        let roster = vec![
            signed(&keys, vec![Tag::parse(["d", &named.to_string()]).unwrap()]),
            signed(
                &keys,
                vec![Tag::parse(["d", &unnamed.to_string()]).unwrap()],
            ),
            // A repeated id must not become a second channel.
            signed(&keys, vec![Tag::parse(["d", &named.to_string()]).unwrap()]),
        ];
        let metadata = vec![
            signed(
                &keys,
                vec![
                    Tag::parse(["d", &named.to_string()]).unwrap(),
                    Tag::parse(["name", "Zebra"]).unwrap(),
                ],
            ),
            // Metadata for a channel the roster does not name is ignored.
            signed(
                &keys,
                vec![
                    Tag::parse(["d", &Uuid::from_u128(3).to_string()]).unwrap(),
                    Tag::parse(["name", "Ghost"]).unwrap(),
                ],
            ),
        ];
        let me = Keys::generate().public_key().to_hex();
        let channels = channels_from(&roster, &metadata, &HashSet::new(), &me);
        let names: Vec<(Uuid, String)> = channels
            .iter()
            .map(|channel| (channel.id, channel.name.clone()))
            .collect();
        // Sorted by lowercase name: the id-as-name entry sorts before "Zebra".
        assert_eq!(
            names,
            vec![(unnamed, unnamed.to_string()), (named, "Zebra".to_owned())]
        );
        assert_eq!(channels.len(), 2, "a repeated id is still one channel");
    }

    #[test]
    fn a_roster_entry_that_is_not_a_uuid_is_skipped() {
        let keys = keys();
        let roster = vec![
            signed(&keys, vec![Tag::parse(["d", "not-a-uuid"]).unwrap()]),
            signed(
                &keys,
                vec![Tag::parse(["d", &Uuid::from_u128(4).to_string()]).unwrap()],
            ),
        ];
        let me = Keys::generate().public_key().to_hex();
        let channels = channels_from(&roster, &[], &HashSet::new(), &me);
        assert_eq!(channels.len(), 1);
        assert_eq!(channels[0].id, Uuid::from_u128(4));
    }

    #[test]
    fn two_events_in_one_second_keep_one_order() {
        let keys = keys();
        let first = at(&keys, 100);
        let second = at(&keys, 100);
        let (low, high) = if first.id.to_hex() < second.id.to_hex() {
            (first.id, second.id)
        } else {
            (second.id, first.id)
        };
        let ordered = order_timeline(vec![second, first]);
        assert_eq!(ordered[0].id, low);
        assert_eq!(ordered[1].id, high);
    }

    #[test]
    fn a_thread_read_rejects_replies_from_another_channel() {
        let keys = keys();
        let selected = channel();
        let other = Uuid::from_u128(99);
        let root = message(&keys, selected, "root", 10);
        let reply = signed(
            &keys,
            vec![
                Tag::parse(["h", &selected.to_string()]).unwrap(),
                e_tag(&root.id, "root"),
            ],
        );
        let wrong = signed(
            &keys,
            vec![
                Tag::parse(["h", &other.to_string()]).unwrap(),
                e_tag(&root.id, "root"),
            ],
        );
        let (replies, partial) = thread_replies(
            vec![wrong, reply.clone()],
            &root.id.to_hex(),
            Some(selected),
        );
        assert!(!partial);
        assert_eq!(ids(&replies), vec![reply.id.to_hex()]);
    }

    #[test]
    fn a_thread_read_puts_the_root_first_and_drops_duplicates() {
        let keys = keys();
        let root = at(&keys, 10);
        let early = at(&keys, 11);
        let late = at(&keys, 12);
        let thread = thread_events(&root, vec![late.clone(), root.clone(), early.clone()]);
        let ids: Vec<EventId> = thread.iter().map(|event| event.id).collect();
        assert_eq!(ids, vec![root.id, early.id, late.id]);
    }

    #[tokio::test]
    async fn empty_content_is_refused_before_a_write_leaves_the_client() {
        let resolved = Resolved {
            keys: keys(),
            http_url: "http://127.0.0.1:1".to_owned(),
            auth_tag: None,
            sources: crate::config::Sources {
                key: crate::config::KeySource::Flag,
                relay: crate::config::RelaySource::Flag,
            },
        };
        let client = Client::new(&resolved);
        for outcome in [
            client.send_message(channel(), "", None, &[]).await,
            client.edit(channel(), at(&resolved.keys, 1).id, "").await,
        ] {
            match outcome {
                WriteOutcome::Refused { category, .. } => {
                    assert_eq!(category, Category::InvalidInput)
                }
                other => panic!("expected Refused, got {other:?}"),
            }
        }
    }

    #[test]
    fn a_malformed_id_is_bad_input() {
        assert_eq!(
            channel_id("nope").unwrap_err().category,
            Category::InvalidInput
        );
        assert_eq!(
            event_id("nope").unwrap_err().category,
            Category::InvalidInput
        );
        assert!(channel_id("7e5faaba-948a-47b5-8ca0-20c6e47953d3").is_ok());
    }
}
