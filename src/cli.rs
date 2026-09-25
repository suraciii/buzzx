//! The one-shot commands: one call, one JSON value, one exit code. The
//! product contract is docs/browse-collab-cli.md; the exit codes are
//! docs/configuration.md. Nothing here holds state between calls.

use std::io::Read;

use buzz_sdk::extract_channel_id;
use clap::Subcommand;
use nostr::Event;
use serde_json::{Value, json};
use uuid::Uuid;

use crate::client::{Client, WriteOutcome, channel_id, event_id};
use crate::config::{self, Resolved};
use crate::content;
use crate::failure::{Category, Failure};

/// The default `messages get --limit`.
const DEFAULT_LIMIT: u64 = 20;

#[derive(Subcommand)]
pub enum ChannelsCommand {
    /// List the channels the identity can access.
    List,
}

#[derive(Subcommand)]
pub enum MessagesCommand {
    /// Read recent messages in one channel, or one event.
    Get {
        /// Channel UUID.
        #[arg(long)]
        channel: Option<String>,
        /// Event id, instead of `--channel`.
        #[arg(long)]
        event: Option<String>,
        /// How many messages to return, newest first.
        #[arg(long)]
        limit: Option<String>,
    },
    /// Read one thread: the root event and its replies.
    Thread {
        /// Root event id.
        #[arg(long)]
        event: Option<String>,
    },
    /// Send a top-level message.
    Send {
        /// Channel UUID.
        #[arg(long)]
        channel: Option<String>,
        /// Message content, or `-` to read it from stdin.
        #[arg(long)]
        content: Option<String>,
    },
    /// Reply to an event. The channel and the thread context come from it.
    Reply {
        /// Event id to answer.
        #[arg(long)]
        event: Option<String>,
        /// Message content, or `-` to read it from stdin.
        #[arg(long)]
        content: Option<String>,
    },
}

pub async fn run_channels(resolved: &Resolved, action: ChannelsCommand) -> i32 {
    let client = Client::new(resolved);
    match action {
        ChannelsCommand::List => match client.channels().await {
            Ok(channels) => {
                let list: Vec<Value> = channels
                    .iter()
                    .map(|channel| {
                        json!({
                            "channel_id": channel.id.to_string(),
                            "name": channel.name,
                        })
                    })
                    .collect();
                print(&Value::Array(list));
                0
            }
            // The channel list is the identity's own: there is no id to name.
            Err(failure) => fail(&failure, None),
        },
    }
}

pub async fn run_messages(resolved: &Resolved, action: MessagesCommand) -> i32 {
    let client = Client::new(resolved);
    match action {
        MessagesCommand::Get {
            channel,
            event,
            limit,
        } => match (channel, event) {
            (Some(channel), None) => read_channel(&client, &channel, limit.as_deref()).await,
            (None, Some(event)) => match limit {
                // A single event has no window to apply a limit to.
                Some(_) => fail(
                    &Failure::invalid_input("--limit applies to --channel reads"),
                    None,
                ),
                None => read_event(&client, &event).await,
            },
            (Some(_), Some(_)) => fail(
                &Failure::invalid_input("--channel and --event are mutually exclusive"),
                None,
            ),
            (None, None) => fail(
                &Failure::invalid_input("--channel or --event is required"),
                None,
            ),
        },
        MessagesCommand::Thread { event } => read_thread(&client, event.as_deref()).await,
        MessagesCommand::Send { channel, content } => {
            let channel = match channel.as_deref() {
                Some(raw) => channel_id(raw),
                None => Err(Failure::invalid_input("--channel is required")),
            };
            let channel = match channel {
                Ok(channel) => channel,
                Err(failure) => return fail(&failure, None),
            };
            let content = match content_of(content.as_deref()) {
                Ok(content) => content,
                Err(failure) => return fail(&failure, Some(("channel_id", &channel.to_string()))),
            };
            write_result(
                client.send_message(channel, &content, None).await,
                channel,
                None,
            )
        }
        MessagesCommand::Reply { event, content } => {
            let target = match event.as_deref() {
                Some(raw) => event_id(raw),
                None => Err(Failure::invalid_input("--event is required")),
            };
            let target = match target {
                Ok(target) => target,
                Err(failure) => {
                    return fail(
                        &failure,
                        Some(("event_id", event.as_deref().unwrap_or_default())),
                    );
                }
            };
            let content = match content_of(content.as_deref()) {
                Ok(content) => content,
                Err(failure) => return fail(&failure, Some(("event_id", &target.to_hex()))),
            };
            // One read, then one write: the routing comes from the event the
            // relay holds, not from what the caller remembers about it.
            let target_hex = target.to_hex();
            let resolved_event = match client.event(target).await {
                Ok(event) => event,
                Err(failure) => return fail(&failure, Some(("event_id", &target_hex))),
            };
            let Some(channel) = extract_channel_id(&resolved_event) else {
                return fail(
                    &Failure::invalid_input(format!("event {target_hex} is not channel-scoped")),
                    Some(("event_id", &target_hex)),
                );
            };
            write_result(
                client.reply(&resolved_event, &content).await,
                channel,
                Some(&target_hex),
            )
        }
    }
}

async fn read_channel(client: &Client, raw: &str, limit: Option<&str>) -> i32 {
    let channel = match channel_id(raw) {
        Ok(channel) => channel,
        Err(failure) => return fail(&failure, Some(("channel_id", raw))),
    };
    let limit = match parse_limit(limit) {
        Ok(limit) => limit,
        Err(failure) => return fail(&failure, Some(("channel_id", &channel.to_string()))),
    };
    match client.history(channel, limit).await {
        Ok(events) => {
            print(&Value::Array(events.iter().map(event_json).collect()));
            0
        }
        Err(failure) => fail(&failure, Some(("channel_id", &channel.to_string()))),
    }
}

async fn read_event(client: &Client, raw: &str) -> i32 {
    let id = match event_id(raw) {
        Ok(id) => id,
        Err(failure) => return fail(&failure, Some(("event_id", raw))),
    };
    match client.event(id).await {
        Ok(event) => {
            print(&event_json(&event));
            0
        }
        Err(failure) => fail(&failure, Some(("event_id", raw))),
    }
}

async fn read_thread(client: &Client, raw: Option<&str>) -> i32 {
    let id = match raw {
        Some(raw) => event_id(raw),
        None => Err(Failure::invalid_input("--event is required")),
    };
    let id = match id {
        Ok(id) => id,
        Err(failure) => return fail(&failure, Some(("event_id", raw.unwrap_or_default()))),
    };
    match client.thread(id).await {
        Ok(events) => {
            print(&Value::Array(events.iter().map(event_json).collect()));
            0
        }
        Err(failure) => fail(&failure, Some(("event_id", &id.to_hex()))),
    }
}

/// One event, as the contract's object: every field present, `null` when the
/// event does not carry it.
fn event_json(event: &Event) -> Value {
    json!({
        "channel_id": extract_channel_id(event).map(|channel| channel.to_string()),
        "event_id": event.id.to_hex(),
        "author": event.pubkey.to_hex(),
        "created_at": event.created_at.as_secs(),
        "thread_root": content::root_of(event),
        "reply_to": content::parent_of(event),
        "content": event.content,
    })
}

/// One write, as the contract's object. A write that is not confirmed names
/// its category and reason so a caller never parses prose.
fn write_json(outcome: &WriteOutcome, channel: Uuid, reply_to: Option<&str>) -> Value {
    let (status, event_id, error) = match outcome {
        WriteOutcome::Stored { event_id } => ("sent_confirmed", Some(event_id.clone()), None),
        WriteOutcome::Refused { category, reason } => ("not_sent", None, Some((*category, reason))),
        WriteOutcome::Unknown { category, reason } => {
            ("sent_unconfirmed", None, Some((*category, reason)))
        }
    };
    let mut value = json!({
        "status": status,
        "event_id": event_id,
        "channel_id": channel.to_string(),
    });
    if let Some(reply_to) = reply_to {
        value["reply_to"] = json!(reply_to);
    }
    if let Some((category, reason)) = error {
        value["error"] = json!(category.as_str());
        value["message"] = json!(reason);
    }
    value
}

fn write_result(outcome: WriteOutcome, channel: Uuid, reply_to: Option<&str>) -> i32 {
    print(&write_json(&outcome, channel, reply_to));
    match outcome {
        WriteOutcome::Stored { .. } => 0,
        // Both failures exit by their category, so the code and the `error`
        // the object prints never disagree.
        WriteOutcome::Refused { category, .. } | WriteOutcome::Unknown { category, .. } => {
            exit_code(category)
        }
    }
}

/// The default `--limit`, or the caller's own. Zero and nonsense are bad
/// input, not an empty read.
fn parse_limit(raw: Option<&str>) -> Result<u64, Failure> {
    match raw {
        None => Ok(DEFAULT_LIMIT),
        Some(text) => text
            .trim()
            .parse::<u64>()
            .ok()
            .filter(|limit| *limit >= 1)
            .ok_or_else(|| Failure::invalid_input(format!("limit must be at least 1: {text}"))),
    }
}

/// The message content: the argument itself, or stdin when the argument is
/// `-`. Stdin keeps every byte, including a trailing newline; an empty or
/// non-UTF-8 stream is bad input before anything is signed.
fn content_of(raw: Option<&str>) -> Result<String, Failure> {
    let Some(raw) = raw else {
        return Err(Failure::invalid_input("--content is required"));
    };
    if raw != "-" {
        return if raw.is_empty() {
            Err(Failure::invalid_input("content is empty"))
        } else {
            Ok(raw.to_owned())
        };
    }
    let mut bytes = Vec::new();
    std::io::stdin()
        .read_to_end(&mut bytes)
        .map_err(|e| Failure::invalid_input(format!("stdin: {e}")))?;
    if bytes.is_empty() {
        return Err(Failure::invalid_input("content from stdin is empty"));
    }
    String::from_utf8(bytes)
        .map_err(|_| Failure::invalid_input("content from stdin is not valid UTF-8"))
}

/// One JSON value per run: an array for a collection, an object for one event
/// or one write. Diagnostics go to stderr.
fn print(value: &Value) {
    println!("{value}");
}

/// A read failure carries the id the command was given or derived, when it
/// has one.
fn fail(failure: &Failure, id: Option<(&str, &str)>) -> i32 {
    print(&failure_json(failure, id));
    eprintln!("buzzx: {}", failure.detail);
    exit_code(failure.category)
}

fn failure_json(failure: &Failure, id: Option<(&str, &str)>) -> Value {
    let mut value = json!({
        "error": failure.category.as_str(),
        "message": failure.detail,
    });
    if let Some((field, text)) = id {
        value[field] = json!(text);
    }
    value
}

fn exit_code(category: Category) -> i32 {
    match category {
        Category::InvalidInput | Category::NotFound => config::EXIT_USAGE,
        Category::Network | Category::TimeoutUnknown => config::EXIT_NETWORK,
        Category::Forbidden => config::EXIT_AUTH,
        Category::RelayRejected => config::EXIT_OTHER,
    }
}

/// A failure raised before a command ran: the identity or the relay did not
/// resolve. It prints the same one-object failure as a failed read, so a
/// caller parses one shape on every path.
pub fn fail_startup(code: i32, message: &str) -> i32 {
    let category = startup_category(code);
    print(&json!({"error": category.as_str(), "message": message}));
    eprintln!("buzzx: {message}");
    exit_code(category)
}

/// The category a startup failure carries. Startup failures are raised
/// before any relay call: code 1 is bad input, code 3 is the identity, and
/// the catch-all code 4 is the relay's own bucket. The category decides the
/// code, so the two cannot disagree.
fn startup_category(code: i32) -> Category {
    match code {
        config::EXIT_USAGE => Category::InvalidInput,
        config::EXIT_AUTH => Category::Forbidden,
        _ => Category::RelayRejected,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use nostr::{EventBuilder, Keys, Kind, Tag};

    fn keys() -> Keys {
        Keys::generate()
    }

    fn channel() -> Uuid {
        Uuid::from_u128(0x7e5f_aaba_948a_47b5_8ca0_20c6_e479_53d3)
    }

    fn message(keys: &Keys, tags: Vec<Tag>) -> Event {
        EventBuilder::new(Kind::Custom(9), "hello")
            .tags(tags)
            .sign_with_keys(keys)
            .unwrap()
    }

    fn h_tag() -> Tag {
        Tag::parse(["h", &channel().to_string()]).unwrap()
    }

    #[test]
    fn an_event_object_carries_every_field() {
        let keys = keys();
        let top = message(&keys, vec![h_tag()]);
        let object = event_json(&top);
        assert_eq!(object["channel_id"], json!(channel().to_string()));
        assert_eq!(object["event_id"], json!(top.id.to_hex()));
        assert_eq!(object["author"], json!(keys.public_key().to_hex()));
        assert_eq!(object["created_at"], json!(top.created_at.as_secs()));
        assert_eq!(object["content"], json!("hello"));
        // A top-level message starts its own thread.
        assert_eq!(object["thread_root"], Value::Null);
        assert_eq!(object["reply_to"], Value::Null);

        let reply = message(
            &keys,
            vec![
                h_tag(),
                Tag::parse(["e", &top.id.to_hex(), "", "reply"]).unwrap(),
            ],
        );
        let nested = message(
            &keys,
            vec![
                h_tag(),
                Tag::parse(["e", &top.id.to_hex(), "", "root"]).unwrap(),
                Tag::parse(["e", &reply.id.to_hex(), "", "reply"]).unwrap(),
            ],
        );
        let object = event_json(&nested);
        assert_eq!(object["thread_root"], json!(top.id.to_hex()));
        assert_eq!(object["reply_to"], json!(reply.id.to_hex()));
    }

    #[test]
    fn an_event_without_a_channel_reports_null() {
        let object = event_json(&message(&keys(), Vec::new()));
        assert_eq!(object["channel_id"], Value::Null);
    }

    #[test]
    fn a_confirmed_write_carries_the_relays_id() {
        let outcome = WriteOutcome::Stored {
            event_id: "ab".repeat(32),
        };
        let object = write_json(&outcome, channel(), Some("cd"));
        assert_eq!(object["status"], json!("sent_confirmed"));
        assert_eq!(object["event_id"], json!("ab".repeat(32)));
        assert_eq!(object["channel_id"], json!(channel().to_string()));
        assert_eq!(object["reply_to"], json!("cd"));
        assert!(object.get("error").is_none());
    }

    #[test]
    fn a_refused_write_names_its_category_and_an_unknown_one_exits_two() {
        let refused = WriteOutcome::Refused {
            category: Category::Forbidden,
            reason: "not a member".to_owned(),
        };
        let object = write_json(&refused, channel(), None);
        assert_eq!(object["status"], json!("not_sent"));
        assert_eq!(object["event_id"], Value::Null);
        assert_eq!(object["error"], json!("forbidden"));
        assert_eq!(object["message"], json!("not a member"));
        assert!(object.get("reply_to").is_none());
        assert_eq!(write_result(refused, channel(), None), config::EXIT_AUTH);

        let unknown = WriteOutcome::Unknown {
            category: Category::TimeoutUnknown,
            reason: "timeout".to_owned(),
        };
        let object = write_json(&unknown, channel(), None);
        assert_eq!(object["status"], json!("sent_unconfirmed"));
        assert_eq!(object["error"], json!("timeout_unknown"));
        assert_eq!(write_result(unknown, channel(), None), config::EXIT_NETWORK);

        // An unconfirmed write exits by its category, so a relay that
        // answered and failed is not reported as a lost answer.
        let failed = WriteOutcome::Unknown {
            category: Category::RelayRejected,
            reason: "HTTP 500: boom".to_owned(),
        };
        let object = write_json(&failed, channel(), None);
        assert_eq!(object["status"], json!("sent_unconfirmed"));
        assert_eq!(object["error"], json!("relay_rejected"));
        assert_eq!(write_result(failed, channel(), None), config::EXIT_OTHER);
    }

    #[test]
    fn a_startup_failure_carries_the_category_of_its_code() {
        assert_eq!(startup_category(config::EXIT_USAGE), Category::InvalidInput);
        assert_eq!(startup_category(config::EXIT_AUTH), Category::Forbidden);
        assert_eq!(
            startup_category(config::EXIT_OTHER),
            Category::RelayRejected
        );
        // The category decides the code the caller sees, so the JSON never
        // names a category whose exit code is different.
        for code in [config::EXIT_USAGE, config::EXIT_AUTH, config::EXIT_OTHER] {
            assert_eq!(exit_code(startup_category(code)), code);
        }
    }

    #[test]
    fn a_read_failure_carries_the_id_it_was_given() {
        let failure = Failure::not_found("no event ab");
        assert_eq!(
            failure_json(&failure, Some(("event_id", "ab"))),
            json!({"error": "not_found", "message": "no event ab", "event_id": "ab"})
        );
        assert_eq!(
            failure_json(&failure, None),
            json!({"error": "not_found", "message": "no event ab"})
        );
        assert_eq!(fail(&failure, None), config::EXIT_USAGE);
    }

    #[test]
    fn a_limit_defaults_and_rejects_nonsense() {
        assert_eq!(parse_limit(None).unwrap(), DEFAULT_LIMIT);
        assert_eq!(parse_limit(Some("5")).unwrap(), 5);
        assert_eq!(
            parse_limit(Some("0")).unwrap_err().category,
            Category::InvalidInput
        );
        assert_eq!(
            parse_limit(Some("many")).unwrap_err().category,
            Category::InvalidInput
        );
    }

    #[test]
    fn every_category_has_the_documented_exit_code() {
        assert_eq!(exit_code(Category::InvalidInput), 1);
        assert_eq!(exit_code(Category::NotFound), 1);
        assert_eq!(exit_code(Category::Network), 2);
        assert_eq!(exit_code(Category::TimeoutUnknown), 2);
        assert_eq!(exit_code(Category::Forbidden), 3);
        assert_eq!(exit_code(Category::RelayRejected), 4);
    }
}
