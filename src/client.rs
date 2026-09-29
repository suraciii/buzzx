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

/// The fields one channel-creation request carries.
///
/// Name, type and visibility are the whole of the first slice: the relay
/// establishes the channel id, the creator's ownership and the initial
/// membership, and member invites, TTL and archival are separate work.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ChannelDraft {
    pub name: String,
    pub kind: buzz_core::channel::ChannelType,
    pub visibility: buzz_core::channel::ChannelVisibility,
    pub description: Option<String>,
}

impl ChannelDraft {
    /// The draft a form submits: the name trimmed, an empty description
    /// dropped. An empty name is bad input before anything is signed.
    pub fn new(
        name: &str,
        kind: buzz_core::channel::ChannelType,
        visibility: buzz_core::channel::ChannelVisibility,
        description: Option<&str>,
    ) -> Result<Self, Failure> {
        let name = name.trim();
        if name.is_empty() {
            return Err(Failure::invalid_input("channel name is required"));
        }
        let description = description
            .map(str::trim)
            .filter(|text| !text.is_empty())
            .map(str::to_owned);
        Ok(Self {
            name: name.to_owned(),
            kind,
            visibility,
            description,
        })
    }
}

/// The outcome of a channel creation.
///
/// The relay, not the client, establishes that the channel exists: a canonical
/// answer confirms it, a refused one means nothing was created, and a lost
/// answer is unconfirmed and must never be retried by itself.
#[derive(Debug, Clone, PartialEq)]
pub enum CreateOutcome {
    /// The relay stored the creation event under the client's channel id.
    Confirmed { channel_id: Uuid },
    /// The write may have happened: the answer was lost. The channel id is
    /// the one the request carried, and the caller refreshes instead of
    /// submitting a duplicate.
    Unconfirmed {
        channel_id: Uuid,
        category: Category,
        reason: String,
    },
    /// Nothing was created: bad input, or the relay refused it.
    Refused { category: Category, reason: String },
}

impl CreateOutcome {
    /// The channel id the request carried, whenever the client chose one. It
    /// is known before the relay answers, so it is present on a refused
    /// request too; only a confirmed or unconfirmed write means the channel
    /// may exist.
    pub fn channel_id(&self) -> Option<Uuid> {
        match self {
            CreateOutcome::Confirmed { channel_id }
            | CreateOutcome::Unconfirmed { channel_id, .. } => Some(*channel_id),
            CreateOutcome::Refused { .. } => None,
        }
    }
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
/// Which side of a loaded history window a page extends.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HistoryDirection {
    Older,
    Newer,
}

/// A bounded message search. `author` is an exact public key when present.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct SearchRequest {
    pub query: String,
    pub channel: Option<Uuid>,
    pub author: Option<String>,
    pub since: Option<u64>,
    pub until: Option<u64>,
}

/// A context read around one exact event.
#[derive(Debug, Clone)]
pub struct ContextRead {
    pub events: Vec<Event>,
    pub before_complete: bool,
    pub after_complete: bool,
}

/// One answered page or window of a timeline: the rows oldest first, and
/// whether the relay's answer reached its bound.
pub struct HistoryPage {
    pub events: Vec<Event>,
    /// The raw answer carried a full page, decided before any filtering
    /// drops rows: a page of a thread whose root check removed half the
    /// answer is still a cut page. True means more rows may exist beyond
    /// this page in its direction, so a caller must not treat it as a
    /// boundary. False is the boundary evidence: the relay exhausted the
    /// range, so nothing more exists there.
    pub saturated: bool,
}

/// How many events one newest-to-anchor walk batch asks for. The bound a
/// catch-up filter already uses: one batch is the most a walk holds at once.
const WALK_LIMIT: u64 = 1000;

/// One newest-to-anchor walk over a `since` range. The relay answers newest
/// first, so the rows adjacent to the cursor sit at the oldest end of the
/// answer, and one bounded query reaches them only while the whole range
/// fits in it. The walk asks for the range's newest end first and steps an
/// inclusive `until` bound down toward the cursor, keeping only the oldest
/// `want` rows it has seen. Memory stays bounded: one batch plus the
/// retained pool, whatever the conversation holds.
struct Walk {
    want: u64,
    /// The batch size this walk decides full pages against.
    batch_limit: u64,
    /// The cursor's second, named by the saturation failure.
    since: u64,
    /// The oldest rows seen so far, canonical order, at most `want` of them.
    pool: Vec<Event>,
    /// A batch answered with a full page: the range holds rows the walk has
    /// not seen, so more exist beyond any page it can hand out.
    saturated: bool,
    /// The second the previous full batch descended to. A full batch that
    /// reaches no further means the identical query would repeat: the wall
    /// the saturation failure names, in the relay's own order as much as in
    /// a packed second.
    bound: Option<u64>,
}

/// What one answered batch means for the walk.
enum WalkStep {
    /// The range is not exhausted: ask again with this second as the next
    /// batch's inclusive `until`.
    Descend(u64),
    /// The cursor is reached: the adjacent rows and the saturation evidence.
    Done(HistoryPage),
}

/// The older-side progress rule. An inclusive `until` cursor that answers a
/// full page sitting entirely at its own second has not moved toward older
/// rows, and asking the identical query again would return the identical
/// page. The page grows until it clears the second, and only a second holding
/// more events than the relay's own bound is reported as a limit.
struct OlderProgress {
    limit: u64,
}

impl OlderProgress {
    fn new(limit: u64) -> Self {
        Self {
            limit: limit.clamp(1, WALK_LIMIT),
        }
    }

    fn limit(&self) -> u64 {
        self.limit
    }

    /// One answered page: `Some(limit)` asks again with a bigger page, `None`
    /// accepts this answer as the reader's page.
    fn step(&mut self, batch: &[Event], cursor: u64, asked: u64) -> Option<u64> {
        let saturated = batch.len() as u64 >= asked;
        let passed = batch
            .iter()
            .any(|event| event.created_at.as_secs() < cursor);
        if !saturated || passed || self.limit >= WALK_LIMIT {
            return None;
        }
        self.limit = (self.limit * 4).min(WALK_LIMIT);
        Some(self.limit)
    }
}

impl Walk {
    fn new(want: u64, since: u64, batch_limit: u64) -> Self {
        Walk {
            want,
            batch_limit,
            since,
            pool: Vec::new(),
            saturated: false,
            bound: None,
        }
    }

    /// Fold one answered batch in. An inclusive bound re-answers its own
    /// boundary second, so batches overlap by identity and the pool
    /// deduplicates instead of dropping the second.
    fn step(&mut self, batch: Vec<Event>) -> Result<WalkStep, Failure> {
        let lowest = batch
            .iter()
            .map(|event| event.created_at.as_secs())
            .min()
            .unwrap_or(self.since);
        let full = batch.len() as u64 >= self.batch_limit;
        self.saturated = self.saturated || full;
        // A full batch that never leaves one second cannot be descended
        // through: stepping the bound to that second asks the identical
        // query, so the rows below it stay out of reach. The same holds for
        // a full batch that reached no further than the last one, whatever
        // the relay's reason. That is the saturated boundary the contract
        // names, and it is reported with its visible gap, never skipped and
        // never walked in circles.
        if full {
            let one_second = batch
                .iter()
                .all(|event| event.created_at.as_secs() == lowest);
            if one_second || self.bound == Some(lowest) {
                return Err(Failure::new(
                    Category::RelayRejected,
                    format!(
                        "History limit reached: second {lowest} holds more than {} events, so the \
                         rows after second {} cannot be read exactly",
                        self.batch_limit, self.since
                    ),
                ));
            }
        }
        let mut seen: HashSet<String> = self.pool.iter().map(|event| event.id.to_hex()).collect();
        let mut merged = std::mem::take(&mut self.pool);
        for event in batch {
            if seen.insert(event.id.to_hex()) {
                merged.push(event);
            }
        }
        let merged = order_timeline(merged);
        if !full {
            // The batch was the rest of the range: what the walk has seen is
            // everything at or after the cursor, so the evidence is exact. A
            // walk that ever saw a full page already knows more exist.
            let more = self.saturated || merged.len() as u64 > self.want;
            return Ok(WalkStep::Done(HistoryPage {
                events: merged.into_iter().take(self.want as usize).collect(),
                saturated: more,
            }));
        }
        self.pool = merged.into_iter().take(self.want as usize).collect();
        // The batch spans at least two seconds, and the older one may still
        // hold rows nearer the cursor: descend to it, inclusively.
        self.bound = Some(lowest);
        Ok(WalkStep::Descend(lowest))
    }
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
    /// Search the relay's supported textual message kinds. The relay owns
    /// matching semantics; this method only constructs the explicit filter.
    pub async fn search(&self, request: &SearchRequest) -> Result<Vec<Event>, Failure> {
        if request.query.trim().is_empty() && request.author.is_none() {
            return Err(Failure::invalid_input(
                "search needs a keyword or an exact author",
            ));
        }
        if let Some(author) = &request.author
            && (author.len() != 64 || !author.bytes().all(|byte| byte.is_ascii_hexdigit()))
        {
            return Err(Failure::invalid_input(
                "author must be a 64-character hex public key",
            ));
        }
        let mut filter = json!({
            "kinds": content::TIMELINE_KINDS,
            "limit": 50,
        });
        if !request.query.is_empty() {
            filter["search"] = json!(request.query);
        }
        if let Some(channel) = request.channel {
            filter["#h"] = json!([channel.to_string()]);
        }
        if let Some(author) = &request.author {
            filter["authors"] = json!([author]);
        }
        if let Some(since) = request.since {
            filter["since"] = json!(since);
        }
        if let Some(until) = request.until {
            filter["until"] = json!(until);
        }
        self.transport.query(&filter).await
    }

    /// Read the exact hit and bounded channel context around it.
    pub async fn context(
        &self,
        target: EventId,
        channel: Uuid,
        radius: u64,
    ) -> Result<ContextRead, Failure> {
        let exact = self.event(target).await?;
        if extract_channel_id(&exact) != Some(channel) {
            return Err(Failure::invalid_input(
                "the search result is not in the selected conversation",
            ));
        }
        // The cursor is second-resolution, so one query cannot distinguish
        // the target from other events in its second. Ask for extra overlap,
        // then trim around the exact event id instead of dropping a same-
        // second neighbor or claiming a complete boundary.
        let query_limit = radius.saturating_mul(2).saturating_add(1).max(1);
        let at = exact.created_at.as_secs();
        let before = self
            .transport
            .query(&json!({
                "kinds": content::TIMELINE_KINDS,
                "#h": [channel.to_string()],
                "until": at,
                "limit": query_limit,
            }))
            .await?;
        let before_saturated = before.len() as u64 >= query_limit;
        // The relay answers newest first, so the nearest later rows take the
        // bounded walk: a plain `since` answer would start at the
        // conversation's newest end and silently skip everything between it
        // and the target.
        let (after, after_saturated) = self.later_rows(channel, None, at, radius).await?;
        let mut events = Vec::with_capacity(before.len() + after.len() + 1);
        events.push(exact.clone());
        events.extend(before);
        events.extend(after);
        let mut unique = HashSet::new();
        events.retain(|event| unique.insert(event.id.to_hex()));
        events.retain(|event| extract_channel_id(event) == Some(channel));
        let events = order_timeline(events);
        let focus = events
            .iter()
            .position(|event| event.id == target)
            .ok_or_else(|| Failure::not_found(format!("no event {}", target.to_hex())))?;
        let radius = radius as usize;
        let start = focus.saturating_sub(radius);
        let end = (focus + radius + 1).min(events.len());
        let before_count = focus - start;
        let after_count = end.saturating_sub(focus + 1);
        let events = events[start..end].to_vec();
        Ok(ContextRead {
            events,
            before_complete: before_count < radius && !before_saturated,
            after_complete: after_count < radius && !after_saturated,
        })
    }

    /// Extend one channel window by one adjacent page. Inclusive relay
    /// cursors are expected: an `until` page re-answers its boundary second,
    /// and the caller deduplicates the overlap by event identity. An older
    /// page grows until it leaves that second, so a full page inside it is
    /// not mistaken for a boundary (see [`Client::older_rows`]). A Newer
    /// page is walked down to the cursor, so it carries the rows that come
    /// right after it instead of the newest rows of the whole range.
    pub async fn history_page(
        &self,
        channel: Uuid,
        direction: HistoryDirection,
        cursor: u64,
        limit: u64,
    ) -> Result<HistoryPage, Failure> {
        match direction {
            HistoryDirection::Older => {
                let (raw, saturated) = self
                    .older_rows(
                        json!({
                            "kinds": content::TIMELINE_KINDS,
                            "#h": [channel.to_string()],
                        }),
                        cursor,
                        limit,
                    )
                    .await?;
                Ok(HistoryPage {
                    events: order_timeline(raw),
                    saturated,
                })
            }
            HistoryDirection::Newer => {
                let (events, saturated) = self.later_rows(channel, None, cursor, limit).await?;
                Ok(HistoryPage { events, saturated })
            }
        }
    }

    /// One older page under an inclusive `until` cursor, grown until it
    /// leaves the cursor's own second or reaches the relay's own bound. The
    /// page a caller gets is the one that moved; `saturated` reports whether
    /// that page was full, which is the only evidence of rows the relay did
    /// not carry.
    async fn older_rows(
        &self,
        filter: Value,
        cursor: u64,
        limit: u64,
    ) -> Result<(Vec<Event>, bool), Failure> {
        let mut progress = OlderProgress::new(limit);
        loop {
            let page = progress.limit();
            let mut filter = filter.clone();
            filter["until"] = json!(cursor);
            filter["limit"] = json!(page);
            let raw = self.transport.query(&filter).await?;
            match progress.step(&raw, cursor, page) {
                Some(next) => debug_assert!(next > page),
                None => {
                    let saturated = raw.len() as u64 >= page;
                    return Ok((raw, saturated));
                }
            }
        }
    }

    /// Extend a thread reply window without treating the old 500-row query
    /// bound as an archive boundary. Saturation is decided on the raw
    /// answer: a full page whose rows partly fail the root check is still a
    /// cut page, and the filtered count below the bound is not boundary
    /// evidence.
    pub async fn thread_page(
        &self,
        root: EventId,
        channel: Uuid,
        direction: HistoryDirection,
        cursor: u64,
        limit: u64,
    ) -> Result<HistoryPage, Failure> {
        let hex = root.to_hex();
        let (raw, saturated) = match direction {
            HistoryDirection::Older => {
                self.older_rows(
                    json!({
                        "kinds": content::TIMELINE_KINDS,
                        "#e": [&hex],
                        "#h": [channel.to_string()],
                    }),
                    cursor,
                    limit,
                )
                .await?
            }
            HistoryDirection::Newer => self.later_rows(channel, Some(root), cursor, limit).await?,
        };
        let (replies, _) = thread_replies(raw, &hex, Some(channel));
        Ok(HistoryPage {
            events: order_timeline(replies),
            saturated,
        })
    }

    /// The rows that come right after one cursor: the newest-to-anchor walk
    /// over the `since` range, returned oldest first with the raw answer's
    /// saturation evidence, before any thread filtering. `root` bounds a
    /// thread page to one reply tree; `None` is a channel page.
    async fn later_rows(
        &self,
        channel: Uuid,
        root: Option<EventId>,
        since: u64,
        want: u64,
    ) -> Result<(Vec<Event>, bool), Failure> {
        let mut filter = json!({
            "kinds": content::TIMELINE_KINDS,
            "#h": [channel.to_string()],
            "since": since,
            "limit": WALK_LIMIT,
        });
        if let Some(root) = root {
            filter["#e"] = json!([root.to_hex()]);
        }
        let mut walk = Walk::new(want, since, WALK_LIMIT);
        loop {
            let batch = self.transport.query(&filter).await?;
            match walk.step(batch)? {
                WalkStep::Descend(until) => filter["until"] = json!(until),
                WalkStep::Done(page) => return Ok((page.events, page.saturated)),
            }
        }
    }

    /// One conversation's newest window: the read behind `G`. The rows are
    /// the newest the conversation holds, oldest first; a full answer may
    /// have cut older rows off, which `saturated` reports.
    pub async fn latest(&self, channel: Uuid, limit: u64) -> Result<HistoryPage, Failure> {
        let events = self.history(channel, limit).await?;
        let saturated = events.len() as u64 >= limit;
        Ok(HistoryPage { events, saturated })
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

    /// The current membership, the member roles and the member profiles a
    /// mention preflight matches names against. A failed read is an error,
    /// never an empty directory: an incomplete roster would drop the
    /// recipients a draft names without saying so.
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
        let roles = rosters
            .first()
            .map(content::member_roles)
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
        Ok(mentions::Directory {
            members,
            profiles,
            roles,
        })
    }

    /// The recipients a draft names in one conversation, or why it cannot be
    /// published. Every surface calls this one preflight: the CLI, the
    /// composer and the browser have no name rules of their own.
    pub async fn plan_mentions(
        &self,
        channel: Uuid,
        content: &str,
        bindings: &[mentions::Binding],
    ) -> Result<Vec<String>, mentions::Block> {
        if !mentions::needs_lookup(content) {
            return Ok(Vec::new());
        }
        match self.mention_directory(channel).await {
            Ok(directory) => mentions::plan(content, &directory, bindings),
            Err(failure) => Err(mentions::Block::LookupFailed {
                reason: failure.detail,
                category: failure.category,
            }),
        }
    }

    /// Create a channel in the active community. The client chooses the id and
    /// the relay establishes the rest: the canonical event, the creator's
    /// ownership and the initial membership. A write whose answer was lost is
    /// unconfirmed, never retried.
    pub async fn create_channel(&self, draft: &ChannelDraft) -> CreateOutcome {
        let channel_id = Uuid::new_v4();
        let builder = match buzz_sdk::builders::build_create_channel(
            channel_id,
            &draft.name,
            Some(draft.visibility),
            Some(draft.kind),
            draft.description.as_deref(),
            None,
        ) {
            Ok(builder) => builder,
            Err(error) => {
                return CreateOutcome::Refused {
                    category: Category::InvalidInput,
                    reason: error.to_string(),
                };
            }
        };
        match self.submit(builder).await {
            WriteOutcome::Stored { .. } => CreateOutcome::Confirmed { channel_id },
            WriteOutcome::Unknown { category, reason } => CreateOutcome::Unconfirmed {
                channel_id,
                category,
                reason,
            },
            WriteOutcome::Refused { category, reason } => {
                CreateOutcome::Refused { category, reason }
            }
        }
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
    /// event itself. `mentions` are the recipients the caller resolved from
    /// the content, exactly as `send_message` reads them.
    pub async fn reply(&self, target: &Event, content: &str, mentions: &[String]) -> WriteOutcome {
        let route = match routing(target) {
            Ok(route) => route,
            Err(failure) => return failure.into(),
        };
        self.send_message(route.channel, content, Some(route.thread), mentions)
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
    fn a_newer_walk_hands_out_the_rows_adjacent_to_the_cursor_not_the_newest() {
        let keys = keys();
        let ch = channel();
        // The relay answers newest first. The batch bound is 3, so this full
        // answer spans seconds and the walk descends to its oldest second,
        // inclusively.
        let batch = vec![
            message(&keys, ch, "m", 22),
            message(&keys, ch, "m", 21),
            message(&keys, ch, "m", 20),
        ];
        let mut walk = Walk::new(2, 10, 3);
        match walk.step(batch).expect("a spanning full page descends") {
            WalkStep::Descend(until) => assert_eq!(until, 20, "the boundary second is kept"),
            WalkStep::Done(_) => panic!("a full page that spans seconds is not the walk's end"),
        }
        // The next batch is partial: the range ends here. What the walk hands
        // out is the oldest it has seen - the rows nearest the cursor - not
        // the newest rows of the range a plain `since` answer would carry.
        let near = vec![
            message(&keys, ch, "later", 19),
            message(&keys, ch, "later", 18),
        ];
        match walk.step(near).expect("a partial batch ends the walk") {
            WalkStep::Done(page) => {
                assert_eq!(page.events.len(), 2);
                assert_eq!(page.events[0].created_at.as_secs(), 18, "oldest first");
                assert_eq!(page.events[1].created_at.as_secs(), 19);
                assert!(
                    page.saturated,
                    "a full page happened: more rows may exist beyond the page"
                );
            }
            WalkStep::Descend(_) => panic!("a partial batch is the walk's end"),
        }
    }

    #[test]
    fn a_range_that_fits_one_batch_is_boundary_evidence() {
        let keys = keys();
        let batch = vec![
            message(&keys, channel(), "m", 12),
            message(&keys, channel(), "m", 11),
        ];
        let mut walk = Walk::new(50, 10, 3);
        match walk.step(batch).expect("a partial batch ends the walk") {
            WalkStep::Done(page) => {
                assert_eq!(page.events.len(), 2);
                assert!(!page.saturated, "the relay exhausted the range");
            }
            WalkStep::Descend(_) => panic!("a partial batch is the walk's end"),
        }
    }

    #[test]
    fn a_page_shorter_than_its_range_reports_more_rows_beyond_it() {
        let keys = keys();
        let batch = vec![
            message(&keys, channel(), "m", 12),
            message(&keys, channel(), "m", 11),
            message(&keys, channel(), "m", 10),
        ];
        let mut walk = Walk::new(2, 10, 4);
        match walk.step(batch).expect("a partial batch ends the walk") {
            WalkStep::Done(page) => {
                assert_eq!(page.events.len(), 2, "the page holds its bound");
                assert!(
                    page.saturated,
                    "the range holds a row the page does not carry"
                );
            }
            WalkStep::Descend(_) => panic!("a partial batch is the walk's end"),
        }
    }

    #[test]
    fn an_inclusive_boundary_second_is_deduplicated_not_dropped() {
        let keys = keys();
        let ch = channel();
        let boundary_a = message(&keys, ch, "boundary a", 20);
        let batch = vec![
            message(&keys, ch, "m", 23),
            message(&keys, ch, "m", 22),
            message(&keys, ch, "m", 21),
            boundary_a.clone(),
        ];
        let mut walk = Walk::new(3, 10, 4);
        match walk.step(batch).expect("a spanning full page descends") {
            WalkStep::Descend(until) => assert_eq!(until, 20),
            WalkStep::Done(_) => panic!("a full page that spans seconds is not the walk's end"),
        }
        // The next batch re-answers the boundary second: the same event
        // again, a different event of that second, and an older row.
        let boundary_b = message(&keys, ch, "boundary b", 20);
        let older = message(&keys, ch, "older", 19);
        let near = vec![older.clone(), boundary_b.clone(), boundary_a.clone()];
        match walk.step(near).expect("a partial batch ends the walk") {
            WalkStep::Done(page) => {
                // Same-second rows keep one canonical order, by id.
                let mut pair = [boundary_a.id.to_hex(), boundary_b.id.to_hex()];
                pair.sort();
                assert_eq!(
                    ids(&page.events),
                    vec![older.id.to_hex(), pair[0].clone(), pair[1].clone()],
                    "the re-answered second adds its new row and drops the duplicate"
                );
                assert!(
                    page.saturated,
                    "the range held five rows and the page carries three"
                );
            }
            WalkStep::Descend(_) => panic!("a partial batch is the walk's end"),
        }
    }

    #[test]
    fn a_full_page_inside_one_second_reports_the_history_limit() {
        let keys = keys();
        // A full answer that never leaves one second cannot be walked
        // through: the bound would ask the identical query.
        let batch: Vec<Event> = (0..3)
            .map(|n| message(&keys, channel(), &format!("burst {n}"), 50))
            .collect();
        let mut walk = Walk::new(2, 10, 3);
        let failure = match walk.step(batch) {
            Err(failure) => failure,
            Ok(_) => panic!("one saturated second cannot be walked through"),
        };
        assert!(
            failure.detail.starts_with("History limit reached"),
            "the saturation failure is explicit: {}",
            failure.detail
        );
        assert!(
            failure.detail.contains("second 50"),
            "the failure names the visible gap: {}",
            failure.detail
        );
    }

    #[test]
    fn an_older_page_stuck_inside_one_second_grows_before_it_is_reported() {
        let keys = keys();
        // Second 50 holds three events; a page of two never leaves it, so the
        // identical query would repeat forever at the same size.
        let batch: Vec<Event> = (0..3)
            .map(|n| message(&keys, channel(), &format!("burst {n}"), 50))
            .collect();
        let mut progress = OlderProgress::new(2);
        assert_eq!(
            progress.step(&batch[..2], 50, 2),
            Some(8),
            "a full page at the cursor's own second asks again with a bigger page"
        );
        assert_eq!(progress.limit(), 8);
        // Three events pass in one page of eight: the bigger page is the one
        // the reader gets.
        assert_eq!(progress.step(&batch, 50, 8), None);
    }

    #[test]
    fn an_older_page_that_passes_the_cursor_second_is_the_readers_page() {
        let keys = keys();
        let cursor = 50;
        let batch = vec![
            message(&keys, channel(), "older", 49),
            message(&keys, channel(), "at the cursor second", 50),
        ];
        let mut progress = OlderProgress::new(2);
        assert_eq!(
            progress.step(&batch, cursor, 2),
            None,
            "a page carrying an older event has passed the boundary second"
        );
    }

    #[test]
    fn an_older_page_at_the_relay_bound_reports_its_limit() {
        let keys = keys();
        let batch: Vec<Event> = (0..4)
            .map(|n| message(&keys, channel(), &format!("burst {n}"), 50))
            .collect();
        // Already at the relay's own bound: growing further would be silently
        // capped, so this answer is the limit the caller must report.
        let mut progress = OlderProgress::new(WALK_LIMIT);
        assert_eq!(progress.step(&batch, 50, 4), None);
        // One growth step below the bound lands exactly on it.
        let mut progress = OlderProgress::new(600);
        assert_eq!(progress.step(&batch, 50, 4), Some(WALK_LIMIT));
    }

    #[test]
    fn a_walk_that_stops_making_progress_reports_the_history_limit() {
        let keys = keys();
        let ch = channel();
        // The narrowed query answered with the same rows again: no
        // second-resolution descent is possible, so the walk names the limit
        // instead of repeating the identical query forever.
        let batch = vec![
            message(&keys, ch, "m", 22),
            message(&keys, ch, "m", 21),
            message(&keys, ch, "m", 20),
        ];
        let mut walk = Walk::new(2, 10, 3);
        match walk
            .step(batch.clone())
            .expect("a spanning full page descends")
        {
            WalkStep::Descend(20) => {}
            _ => panic!("a full page that spans seconds descends"),
        }
        let failure = match walk.step(batch) {
            Err(failure) => failure,
            Ok(_) => panic!("an identical full page is a wall, not progress"),
        };
        assert!(
            failure.detail.starts_with("History limit reached"),
            "the stall is explicit: {}",
            failure.detail
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
            community: None,
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
