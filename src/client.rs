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
use serde_json::json;
use uuid::Uuid;

use crate::config::Resolved;
use crate::content;
use crate::failure::{Category, Failure};
use crate::http::HttpTransport;

pub use crate::http::WriteOutcome;

/// The tag that names a channel id on membership and metadata events.
const D_TAG: &str = "d";
/// The tag that names a channel's display name on metadata events.
const NAME_TAG: &str = "name";
/// How many channel ids one membership or metadata query carries.
const CHANNEL_QUERY_LIMIT: u64 = 500;
/// How many replies one thread read carries.
const THREAD_LIMIT: u64 = 500;
/// How many authors one profile query carries.
const PROFILE_CHUNK: usize = 50;
/// How many authors one profile read resolves. Beyond this, a name stays a
/// short key.
const PROFILE_LIMIT: usize = 200;

#[derive(Debug, Clone, PartialEq)]
pub struct ChannelInfo {
    pub id: Uuid,
    pub name: String,
}

/// Where a reply to one event goes: its channel, and the NIP-10 thread
/// context of the event being answered.
pub struct Routing {
    pub channel: Uuid,
    pub thread: ThreadRef,
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

    /// The channels the identity belongs to: the membership roster, then the
    /// metadata that names them. One entry per roster id; a name that is
    /// missing, or a metadata query that fails, never shortens the list.
    pub async fn channels(&self) -> Result<Vec<ChannelInfo>, Failure> {
        let me = self.keys.public_key().to_hex();
        let roster = self
            .transport
            .query(&json!({
                "kinds": [content::MEMBERSHIP_KIND],
                "#p": [me],
                "limit": CHANNEL_QUERY_LIMIT,
            }))
            .await?;
        let ids = roster_ids(&roster);
        if ids.is_empty() {
            return Ok(Vec::new());
        }
        let described = self
            .transport
            .query(&json!({
                "kinds": [content::CHANNEL_METADATA_KIND],
                "#d": ids,
                "limit": CHANNEL_QUERY_LIMIT,
            }))
            .await?;
        Ok(channels_from(&roster, &described))
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
    pub async fn thread(&self, root: EventId) -> Result<Vec<Event>, Failure> {
        let hex = root.to_hex();
        let mut found = self
            .transport
            .query(&json!({ "ids": [hex], "limit": 1 }))
            .await?;
        let Some(root_event) = found.pop() else {
            return Err(Failure::not_found(format!("no event {hex}")));
        };
        let replies = self
            .transport
            .query(&json!({
                "kinds": content::TIMELINE_KINDS,
                "#e": [hex],
                "limit": THREAD_LIMIT,
            }))
            .await?;
        Ok(thread_events(&root_event, replies))
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

    /// A top-level message, or a reply when `thread` is given.
    pub async fn send_message(
        &self,
        channel: Uuid,
        content: &str,
        thread: Option<ThreadRef>,
    ) -> WriteOutcome {
        if content.is_empty() {
            return refused(Category::InvalidInput, "content is empty".to_owned());
        }
        match build_message(channel, content, thread.as_ref(), &[], false, &[]) {
            Ok(builder) => self.submit(builder).await,
            Err(e) => refused(Category::InvalidInput, e.to_string()),
        }
    }

    /// Answer one event: its channel and its thread context, derived from the
    /// event itself.
    pub async fn reply(&self, target: &Event, content: &str) -> WriteOutcome {
        let route = match routing(target) {
            Ok(route) => route,
            Err(failure) => return failure.into(),
        };
        self.send_message(route.channel, content, Some(route.thread))
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
    event.tags.iter().find_map(|tag| {
        let parts = tag.as_slice();
        (parts.first().map(String::as_str) == Some(name))
            .then(|| parts.get(1).cloned())
            .flatten()
    })
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

/// One entry per roster id. The metadata names the channel when it has an
/// entry for it; otherwise the id stands in for the name.
fn channels_from(roster: &[Event], metadata: &[Event]) -> Vec<ChannelInfo> {
    let mut names: HashMap<String, String> = HashMap::new();
    for event in metadata {
        let Some(id) = first_tag(event, D_TAG) else {
            continue;
        };
        let Some(name) = first_tag(event, NAME_TAG).filter(|name| !name.is_empty()) else {
            continue;
        };
        names.entry(id).or_insert(name);
    }
    let mut channels: Vec<ChannelInfo> = roster_ids(roster)
        .into_iter()
        .filter_map(|id| Uuid::parse_str(&id).ok().map(|uuid| (id, uuid)))
        .map(|(id, uuid)| {
            let name = names.get(&id).cloned().unwrap_or_else(|| id.clone());
            ChannelInfo { id: uuid, name }
        })
        .collect();
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
        let channels = channels_from(&roster, &metadata);
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
        let channels = channels_from(&roster, &[]);
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
            client.send_message(channel(), "", None).await,
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
