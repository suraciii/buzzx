//! The identity's encrypted read-state snapshots. Relay querying and timestamps
//! belong to the caller; this module only builds and merges protocol events.

use std::collections::HashMap;

use nostr::nips::nip44::{self, Version};
use nostr::{Event, EventBuilder, Keys, Kind, Tag};
use serde::Deserialize;
use serde_json::{Map, Value};
use sha2::{Digest, Sha256};

use crate::content::READ_STATE_KIND;

/// This client's identity in the shared protocol.
pub const CLIENT_ID: &str = "buzzx";

const SLOT_PREFIX: &str = "read-state:";
const TYPE_TAG: &str = "read-state";

/// The `d` tag value this client owns, derived deterministically from its public key.
pub fn slot(pubkey_hex: &str) -> String {
    let digest = Sha256::digest(pubkey_hex.as_bytes());
    format!("{SLOT_PREFIX}{}", hex::encode(&digest[..16]))
}

/// The merged read-state contexts and whether any owned slot could not be read.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ReadState {
    pub contexts: HashMap<String, u64>,
    pub newest: u64,
    pub gaps: bool,
}

#[derive(Deserialize)]
struct Payload {
    #[serde(default = "version_one")]
    v: u64,
    client_id: String,
    contexts: HashMap<String, u64>,
}

fn version_one() -> u64 {
    1
}

/// Merge every read-state event owned by this identity.
pub fn parse(keys: &Keys, events: &[Event]) -> ReadState {
    let author = keys.public_key();
    let mut result = ReadState {
        contexts: HashMap::new(),
        newest: 0,
        gaps: false,
    };

    for event in events {
        if event.pubkey != author
            || u32::from(event.kind.as_u16()) != READ_STATE_KIND
            || !has_read_state_slot(event)
        {
            continue;
        }

        result.newest = result.newest.max(event.created_at.as_secs());
        let Ok(plaintext) = nip44::decrypt(keys.secret_key(), &author, &event.content) else {
            result.gaps = true;
            continue;
        };
        let Ok(payload) = serde_json::from_str::<Payload>(&plaintext) else {
            result.gaps = true;
            continue;
        };
        if payload.v != 1 || payload.client_id.len() > 64 {
            result.gaps = true;
            continue;
        }

        for (context, timestamp) in payload.contexts {
            result
                .contexts
                .entry(context)
                .and_modify(|current| *current = (*current).max(timestamp))
                .or_insert(timestamp);
        }
    }

    result
}

fn has_read_state_slot(event: &Event) -> bool {
    event.tags.iter().any(|tag| {
        let parts = tag.as_slice();
        parts.first().map(String::as_str) == Some("d")
            && parts
                .get(1)
                .is_some_and(|value| value.starts_with(SLOT_PREFIX))
    })
}

/// Build the next encrypted read-state event for this identity.
pub fn builder(keys: &Keys, contexts: &HashMap<String, u64>) -> Result<EventBuilder, String> {
    let contexts = serde_json::to_value(contexts)
        .map_err(|error| format!("encode read-state contexts: {error}"))?;
    let mut payload = Map::new();
    payload.insert("v".to_owned(), Value::from(1));
    payload.insert("client_id".to_owned(), Value::from(CLIENT_ID));
    payload.insert("contexts".to_owned(), contexts);
    let plaintext = serde_json::to_string(&Value::Object(payload))
        .map_err(|error| format!("encode read-state payload: {error}"))?;
    let encrypted = nip44::encrypt(
        keys.secret_key(),
        &keys.public_key(),
        &plaintext,
        Version::V2,
    )
    .map_err(|error| format!("encrypt read-state payload: {error}"))?;

    let slot = slot(&keys.public_key().to_hex());
    let tags = vec![
        Tag::parse(["d", slot.as_str()]).map_err(|error| error.to_string())?,
        Tag::parse(["t", TYPE_TAG]).map_err(|error| error.to_string())?,
    ];
    Ok(EventBuilder::new(Kind::Custom(READ_STATE_KIND as u16), encrypted).tags(tags))
}

#[cfg(test)]
mod tests {
    use super::*;
    use nostr::Timestamp;

    fn keys() -> Keys {
        Keys::generate()
    }

    fn signed_at(builder: EventBuilder, keys: &Keys, seconds: u64) -> Event {
        builder
            .custom_created_at(Timestamp::from_secs(seconds))
            .sign_with_keys(keys)
            .expect("signing a read-state test event")
    }

    fn signed(keys: &Keys, contexts: &HashMap<String, u64>, seconds: u64) -> Event {
        signed_at(builder(keys, contexts).unwrap(), keys, seconds)
    }

    fn signed_in_slot(
        keys: &Keys,
        contexts: &HashMap<String, u64>,
        slot: &str,
        seconds: u64,
    ) -> Event {
        let tags = vec![
            Tag::parse(["d", slot]).unwrap(),
            Tag::parse(["t", TYPE_TAG]).unwrap(),
        ];
        signed_at(builder(keys, contexts).unwrap().tags(tags), keys, seconds)
    }

    fn corrupted(event: &Event, keys: &Keys, seconds: u64) -> Event {
        signed_at(
            EventBuilder::new(Kind::Custom(READ_STATE_KIND as u16), "%%% not base64 %%%")
                .tags(event.tags.clone()),
            keys,
            seconds,
        )
    }

    fn contexts(entries: &[(&str, u64)]) -> HashMap<String, u64> {
        entries
            .iter()
            .map(|(key, timestamp)| ((*key).to_owned(), *timestamp))
            .collect()
    }

    #[test]
    fn builder_round_trips_encrypted_contexts() {
        let keys = keys();
        let channel = "8f8e6a78-0021-48c9-9ad0-2c74ad165af5";
        let values = contexts(&[(channel, 1_700_000_000)]);
        let event = signed(&keys, &values, 10);

        assert!(!event.content.contains(channel));
        assert!(!event.content.contains("contexts"));
        assert!(serde_json::from_str::<Value>(&event.content).is_err());
        let state = parse(&keys, &[event]);
        assert_eq!(state.contexts, values);
        assert!(!state.gaps);
    }

    #[test]
    fn merge_takes_each_context_maximum_and_newest_event_time() {
        let keys = keys();
        let older = signed(&keys, &contexts(&[("channel-a", 20), ("msg:abc", 4)]), 50);
        let newer = signed_in_slot(
            &keys,
            &contexts(&[("channel-a", 12), ("thread:xyz", 7)]),
            "read-state:0123456789abcdef0123456789abcdef",
            80,
        );

        let state = parse(&keys, &[older, newer]);
        assert_eq!(state.contexts["channel-a"], 20);
        assert_eq!(state.contexts["msg:abc"], 4);
        assert_eq!(state.contexts["thread:xyz"], 7);
        assert_eq!(state.newest, 80);
        assert!(!state.gaps);
    }

    #[test]
    fn corrupted_slot_sets_gap_and_keeps_other_slot_contexts() {
        let keys = keys();
        let good = signed(&keys, &contexts(&[("channel-a", 91)]), 20);
        let bad = corrupted(&good, &keys, 40);

        let state = parse(&keys, &[good, bad]);
        assert_eq!(state.contexts["channel-a"], 91);
        assert_eq!(state.newest, 40);
        assert!(state.gaps);
    }

    #[test]
    fn ignores_other_authors_and_non_slot_events() {
        let owner = keys();
        let other_keys = keys();
        let values = contexts(&[("channel-a", 8)]);
        let own = signed(&owner, &values, 10);
        let other_author = signed_at(
            EventBuilder::new(Kind::Custom(READ_STATE_KIND as u16), "%%%").tags(vec![
                Tag::parse(["d", slot(&owner.public_key().to_hex()).as_str()]).unwrap(),
            ]),
            &other_keys,
            30,
        );
        let non_slot = signed_at(
            EventBuilder::new(Kind::Custom(READ_STATE_KIND as u16), "%%%")
                .tags(vec![Tag::parse(["d", "unrelated"]).unwrap()]),
            &owner,
            40,
        );

        let state = parse(&owner, &[other_author, non_slot, own]);
        assert_eq!(state.contexts, values);
        assert_eq!(state.newest, 10);
        assert!(!state.gaps);
    }

    #[test]
    fn slot_is_stable_and_has_protocol_shape() {
        let first = keys();
        let second = keys();
        let first_slot = slot(&first.public_key().to_hex());
        let second_slot = slot(&second.public_key().to_hex());

        assert_eq!(first_slot, slot(&first.public_key().to_hex()));
        let digest = first_slot.strip_prefix(SLOT_PREFIX).unwrap();
        assert_eq!(digest.len(), 32);
        assert!(
            digest
                .bytes()
                .all(|byte| byte.is_ascii_hexdigit() && !byte.is_ascii_uppercase())
        );
        assert_ne!(first_slot, second_slot);
    }

    #[test]
    fn empty_context_map_round_trips() {
        let keys = keys();
        let event = signed(&keys, &HashMap::new(), 10);
        let state = parse(&keys, &[event]);

        assert!(state.contexts.is_empty());
        assert_eq!(state.newest, 10);
        assert!(!state.gaps);
    }

    #[test]
    fn opaque_context_keys_pass_through_unchanged() {
        let keys = keys();
        let values = contexts(&[("msg:01HZX9ABC", 123), ("thread:event-id", 456)]);
        let state = parse(&keys, &[signed(&keys, &values, 10)]);

        assert_eq!(state.contexts, values);
    }

    #[test]
    fn missing_version_and_unknown_fields_are_accepted() {
        let keys = keys();
        let plaintext = r#"{"client_id":"desktop","contexts":{"channel-a":5},"future":true}"#;
        let encrypted = nip44::encrypt(
            keys.secret_key(),
            &keys.public_key(),
            plaintext,
            Version::V2,
        )
        .unwrap();
        let event = signed_at(
            EventBuilder::new(Kind::Custom(READ_STATE_KIND as u16), encrypted).tags(vec![
                Tag::parse(["d", "read-state:0123456789abcdef0123456789abcdef"]).unwrap(),
            ]),
            &keys,
            10,
        );

        let state = parse(&keys, &[event]);
        assert_eq!(state.contexts["channel-a"], 5);
        assert!(!state.gaps);
    }

    #[test]
    fn malformed_context_values_mark_a_gap() {
        let keys = keys();
        let plaintext = r#"{"v":1,"client_id":"desktop","contexts":{"channel-a":"5"}}"#;
        let encrypted = nip44::encrypt(
            keys.secret_key(),
            &keys.public_key(),
            plaintext,
            Version::V2,
        )
        .unwrap();
        let event = signed_at(
            EventBuilder::new(Kind::Custom(READ_STATE_KIND as u16), encrypted).tags(vec![
                Tag::parse(["d", "read-state:0123456789abcdef0123456789abcdef"]).unwrap(),
            ]),
            &keys,
            10,
        );

        let state = parse(&keys, &[event]);
        assert!(state.contexts.is_empty());
        assert!(state.gaps);
    }

    #[test]
    fn malformed_json_and_payload_shapes_mark_gaps() {
        let keys = keys();
        for plaintext in [
            "not json",
            "[]",
            r#"{"v":1,"client_id":"desktop"}"#,
            r#"{"v":1,"client_id":"desktop","contexts":[]}"#,
        ] {
            let encrypted = nip44::encrypt(
                keys.secret_key(),
                &keys.public_key(),
                plaintext,
                Version::V2,
            )
            .unwrap();
            let event = signed_at(
                EventBuilder::new(Kind::Custom(READ_STATE_KIND as u16), encrypted).tags(vec![
                    Tag::parse(["d", "read-state:0123456789abcdef0123456789abcdef"]).unwrap(),
                ]),
                &keys,
                10,
            );

            let state = parse(&keys, &[event]);
            assert!(
                state.contexts.is_empty(),
                "accepted malformed payload: {plaintext}"
            );
            assert!(state.gaps, "missed malformed payload: {plaintext}");
        }
    }
}
