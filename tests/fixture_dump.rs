//! Throwaway fixture generator for the TUI fake-relay evidence run.
//! Run with FIXTURE_OUT=path cargo test --test fixture_dump -- --nocapture.

use nostr::{Event, EventBuilder, Keys, Kind, Tag};
use std::time::{SystemTime, UNIX_EPOCH};

fn main() {}

#[cfg(test)]
mod tests {
    use super::*;

    fn now() -> u64 {
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_secs()
    }

    fn message(keys: &Keys, channel: &str, body: &str, at: u64) -> Event {
        EventBuilder::new(Kind::Custom(9), body)
            .tags(vec![Tag::parse(["h", channel]).unwrap()])
            .custom_created_at(nostr::Timestamp::from_secs(at))
            .sign_with_keys(keys)
            .unwrap()
    }

    #[test]
    fn dump_fixture() {
        let out = match std::env::var("FIXTURE_OUT") {
            Ok(path) => path,
            Err(_) => return,
        };
        let user = Keys::generate();
        let peer = Keys::generate();
        let relay_key = Keys::generate();
        let channel = "9af1c0de-0000-4000-8000-000000000001";
        let base = now() - 600;

        let membership = EventBuilder::new(Kind::Custom(39002), "")
            .tags(vec![
                Tag::parse(["d", channel]).unwrap(),
                Tag::parse(["p", &user.public_key().to_hex()]).unwrap(),
            ])
            .sign_with_keys(&relay_key)
            .unwrap();
        let metadata = EventBuilder::new(Kind::Custom(39000), "")
            .tags(vec![
                Tag::parse(["d", channel]).unwrap(),
                Tag::parse(["name", "evidence"]).unwrap(),
                Tag::parse(["t", "forum"]).unwrap(),
            ])
            .sign_with_keys(&relay_key)
            .unwrap();

        // 120 messages: the first history read shows the newest 100, so the
        // oldest 20 arrive only through an older-page request.
        let mut events = vec![membership, metadata];
        for index in 0..120u64 {
            let at = base + index;
            let body = if index == 30 {
                "the migration decision: keep the marker window at seven days"
            } else {
                "filler row"
            };
            let author = if index % 3 == 0 { &user } else { &peer };
            events.push(message(author, channel, body, at));
        }

        let fixture = serde_json::json!({
            "user_secret": user.secret_key().to_secret_hex(),
            "user_pubkey": user.public_key().to_hex(),
            "channel": channel,
            "events": events,
        });
        std::fs::write(&out, serde_json::to_string(&fixture).unwrap()).unwrap();
        println!("fixture written to {out}");
    }
}
