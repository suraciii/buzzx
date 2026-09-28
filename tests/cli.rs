//! The five one-shot commands against a local fake relay: the JSON on stdout
//! and the exit codes. The product contract is docs/browse-collab-cli.md.
//!
//! The fake relay answers `/query` by applying the filter to the events the
//! test seeded, and `/events` by recording the body and answering with the
//! status the test chose. It admits a request only when its NIP-98 header
//! covers the method, the URL, and the body, and it stores a write only when
//! the event verifies, so a client that signs the wrong thing fails here the
//! way the relay would refuse it.

use std::io::{BufRead, BufReader, Read, Write};
use std::net::{TcpListener, TcpStream};
use std::process::{Command, Stdio};
use std::sync::Arc;
use std::thread;

use base64::Engine;
use nostr::{Event, EventBuilder, Keys, Kind, Tag};
use parking_lot::Mutex;
use serde_json::{Value, json};
use sha2::{Digest, Sha256};

/// The channel every seeded event belongs to.
const CHANNEL: &str = "7e5faaba-948a-47b5-8ca0-20c6e47953d3";
/// A second channel, to prove a filter does not leak between them.
const OTHER_CHANNEL: &str = "2c1e6c1a-1a2b-4f3a-9c1d-0f1e2d3c4b5a";
/// A loopback address nothing listens on: the spawn that must not read the
/// network still names a relay, so the ambient one cannot be reached.
const UNREACHABLE: &str = "http://127.0.0.1:1";

/// What the fake relay does with a write.
#[derive(Clone, Copy, PartialEq)]
enum WriteAnswer {
    /// Store it and answer with the relay's canonical id.
    Stored,
    /// Store it and answer with a different id than the signed one.
    Recanonicalized,
    /// Refuse it: nothing was stored.
    Refused,
    /// Refuse it without naming the identity: nothing was stored either.
    RefusedBadRequest,
    /// Fail after receiving the event: storage is not established.
    ServerError,
    /// Fail after the relay may have accepted it.
    Lost,
}

struct FakeRelay {
    url: String,
    state: Arc<Mutex<State>>,
}

struct State {
    events: Vec<Value>,
    writes: Vec<Value>,
    queries: usize,
    answer: WriteAnswer,
    /// How many reads answer with a dropped connection instead of a body.
    drop_reads: usize,
}

impl FakeRelay {
    fn start() -> FakeRelay {
        let listener = TcpListener::bind("127.0.0.1:0").expect("a free port");
        let url = format!("http://{}", listener.local_addr().unwrap());
        let state = Arc::new(Mutex::new(State {
            events: Vec::new(),
            writes: Vec::new(),
            queries: 0,
            answer: WriteAnswer::Stored,
            drop_reads: 0,
        }));
        let served = Arc::clone(&state);
        let base = url.clone();
        thread::spawn(move || {
            for stream in listener.incoming() {
                match stream {
                    Ok(stream) => {
                        let state = Arc::clone(&served);
                        let base = base.clone();
                        thread::spawn(move || serve(stream, &state, &base));
                    }
                    Err(_) => break,
                }
            }
        });
        FakeRelay { url, state }
    }

    fn seed(&self, event: &Event) {
        self.state
            .lock()
            .events
            .push(serde_json::to_value(event).unwrap());
    }

    /// Seed a row the relay holds but the client cannot read as an event.
    fn seed_unreadable(&self, row: Value) {
        self.state.lock().events.push(row);
    }

    fn answer(&self, answer: WriteAnswer) {
        self.state.lock().answer = answer;
    }

    fn writes(&self) -> Vec<Value> {
        self.state.lock().writes.clone()
    }

    fn queries(&self) -> usize {
        self.state.lock().queries
    }

    /// Drop the answer to the next `count` reads: the caller sees a lost
    /// answer, not a refusal.
    fn drop_reads(&self, count: usize) {
        self.state.lock().drop_reads = count;
    }
}

fn serve(stream: TcpStream, state: &Arc<Mutex<State>>, base: &str) {
    let mut reader = BufReader::new(stream.try_clone().unwrap());
    let mut request_line = String::new();
    if reader.read_line(&mut request_line).is_err() {
        return;
    }
    let mut line_parts = request_line.split_whitespace();
    let method = line_parts.next().unwrap_or("").to_owned();
    let path = line_parts.next().unwrap_or("").to_owned();
    let mut headers: Vec<String> = Vec::new();
    loop {
        let mut line = String::new();
        if reader.read_line(&mut line).unwrap_or(0) == 0 || line.trim().is_empty() {
            break;
        }
        headers.push(line.trim_end().to_owned());
    }
    let length = headers
        .iter()
        .find_map(|line| {
            line.to_lowercase()
                .strip_prefix("content-length:")
                .map(|value| value.trim().parse().unwrap_or(0))
        })
        .unwrap_or(0);
    let mut body = vec![0u8; length];
    let _ = reader.read_exact(&mut body);
    // The relay admits a request only when its NIP-98 header covers this
    // method, this URL, and this body.
    if !nip98_covers(&headers, base, &method, &path, &body) {
        let _ = respond(stream, 401, &json!({"error": "nip-98"}));
        return;
    }
    let request: Value = serde_json::from_slice(&body).unwrap_or(Value::Null);
    let (status, answer) = match path.as_str() {
        "/query" => {
            let drop_answer = {
                let mut state = state.lock();
                state.queries += 1;
                if state.drop_reads > 0 {
                    state.drop_reads -= 1;
                    true
                } else {
                    false
                }
            };
            if drop_answer {
                // Closing without a body is a lost answer, not a refusal.
                return;
            }
            (200, query(state, &request))
        }
        "/events" => {
            let mut state = state.lock();
            state.writes.push(request.clone());
            // The relay stores signed events only: an unsigned body, or one
            // altered on the way, is refused.
            let signed = serde_json::from_value::<Event>(request)
                .ok()
                .filter(|event| event.verify().is_ok())
                .map(|event| event.id.to_hex());
            match signed {
                None => (400, json!({"error": "bad signature"})),
                Some(id) => match state.answer {
                    WriteAnswer::Stored => (200, json!({"event_id": id})),
                    WriteAnswer::Recanonicalized => (200, json!({"event_id": "ab".repeat(32)})),
                    WriteAnswer::Refused => (403, json!({"error": "not a member"})),
                    WriteAnswer::RefusedBadRequest => (400, json!({"error": "bad tag"})),
                    WriteAnswer::ServerError => (500, json!({"error": "storage failed"})),
                    WriteAnswer::Lost => (502, json!({"error": "upstream unreachable"})),
                },
            }
        }
        _ => (404, json!({"error": "no such path"})),
    };
    let _ = respond(stream, status, &answer);
}

/// Whether the request carries a NIP-98 header that covers this method, this
/// URL, and this body, signed by the identity it names.
fn nip98_covers(headers: &[String], base: &str, method: &str, path: &str, body: &[u8]) -> bool {
    let Some(header) = headers.iter().find_map(|line| {
        let (name, value) = line.split_once(':')?;
        name.eq_ignore_ascii_case("authorization")
            .then(|| value.trim().to_owned())
    }) else {
        return false;
    };
    let Some(encoded) = header.strip_prefix("Nostr ") else {
        return false;
    };
    let Ok(json) = base64::engine::general_purpose::STANDARD.decode(encoded) else {
        return false;
    };
    let Ok(event) = serde_json::from_slice::<Event>(&json) else {
        return false;
    };
    if event.kind.as_u16() != 27235 || event.verify().is_err() {
        return false;
    }
    let tag = |name: &str| {
        event.tags.iter().find_map(|tag| {
            let parts = tag.as_slice();
            (parts.first().map(String::as_str) == Some(name))
                .then(|| parts.get(1).cloned())
                .flatten()
        })
    };
    let url = format!("{base}{path}");
    let payload = hex::encode(Sha256::digest(body));
    tag("u").as_deref() == Some(url.as_str())
        && tag("method").as_deref() == Some(method)
        && tag("payload").as_deref() == Some(payload.as_str())
}

fn respond(mut stream: TcpStream, status: u16, answer: &Value) -> std::io::Result<()> {
    let body = answer.to_string();
    let response = format!(
        "HTTP/1.1 {status} OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
        body.len()
    );
    stream.write_all(response.as_bytes())?;
    stream.flush()
}

/// The relay's `POST /query`: apply the one filter to the seeded events, then
/// answer newest first, cut to `limit`, the way a relay does.
fn query(state: &Arc<Mutex<State>>, request: &Value) -> Value {
    let filter = request.get(0).cloned().unwrap_or(Value::Null);
    let state = state.lock();
    let mut matched: Vec<Value> = state
        .events
        .iter()
        .filter(|event| matches_filter(event, &filter))
        .cloned()
        .collect();
    matched.sort_by_key(|event| std::cmp::Reverse(event["created_at"].as_u64().unwrap_or(0)));
    if let Some(limit) = filter.get("limit").and_then(Value::as_u64) {
        matched.truncate(limit as usize);
    }
    Value::Array(matched)
}

fn matches_filter(event: &Value, filter: &Value) -> bool {
    let list = |name: &str| filter.get(name).and_then(Value::as_array).cloned();
    if let Some(kinds) = list("kinds")
        && !kinds.contains(&event["kind"])
    {
        return false;
    }
    if let Some(ids) = list("ids")
        && !ids.contains(&event["id"])
    {
        return false;
    }
    if let Some(authors) = list("authors")
        && !authors.contains(&event["pubkey"])
    {
        return false;
    }
    for (field, tag) in [("#h", "h"), ("#e", "e"), ("#d", "d"), ("#p", "p")] {
        if let Some(values) = list(field) {
            let carries = event["tags"].as_array().is_some_and(|tags| {
                tags.iter().any(|t| {
                    t.get(0).and_then(Value::as_str) == Some(tag)
                        && t.get(1).is_some_and(|value| values.contains(value))
                })
            });
            if !carries {
                return false;
            }
        }
    }
    true
}

/// A spawn that cannot see the operator's environment: the relay is pinned,
/// the config file is absent, and both credentials are removed. A test that
/// reached the ambient relay would send a real message with a real identity,
/// so no test spawns the binary any other way.
fn binary(relay: &str) -> Command {
    let mut command = Command::new(env!("CARGO_BIN_EXE_buzzx"));
    command
        .env("BUZZ_RELAY_URL", relay)
        .env("BUZZX_CONFIG", "/nonexistent/buzzx-test-config")
        .env_remove("BUZZ_PRIVATE_KEY")
        .env_remove("BUZZ_AUTH_TAG");
    command
}

/// Run one command against the relay. Returns the exit code, stdout as JSON,
/// and stderr.
fn run(relay: &str, keys: &Keys, args: &[&str], stdin: Option<&str>) -> (i32, Value, String) {
    let mut command = binary(relay);
    command
        .args(args)
        .env("BUZZ_PRIVATE_KEY", keys.secret_key().to_secret_hex())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    if stdin.is_some() {
        command.stdin(Stdio::piped());
    } else {
        command.stdin(Stdio::null());
    }
    let mut child = command.spawn().expect("the buzzx binary");
    if let Some(text) = stdin {
        child
            .stdin
            .as_mut()
            .expect("piped stdin")
            .write_all(text.as_bytes())
            .expect("writing stdin");
    }
    let output = child.wait_with_output().expect("the command's output");
    let stdout = String::from_utf8(output.stdout).expect("stdout is UTF-8");
    let json = serde_json::from_str(stdout.trim()).unwrap_or_else(|e| {
        panic!("stdout is not one JSON value ({e}): {stdout:?}");
    });
    (
        output.status.code().expect("an exit code"),
        json,
        String::from_utf8_lossy(&output.stderr).into_owned(),
    )
}

fn keys() -> Keys {
    Keys::generate()
}

fn event(keys: &Keys, kind: u32, tags: Vec<Tag>, content: &str, at: u64) -> Event {
    EventBuilder::new(Kind::Custom(kind as u16), content)
        .tags(tags)
        .custom_created_at(nostr::Timestamp::from_secs(at))
        .sign_with_keys(keys)
        .expect("signing a test event")
}

fn h_tag(channel: &str) -> Tag {
    Tag::parse(["h", channel]).unwrap()
}

fn d_tag(channel: &str) -> Tag {
    Tag::parse(["d", channel]).unwrap()
}

fn message(keys: &Keys, content: &str, at: u64) -> Event {
    event(keys, 9, vec![h_tag(CHANNEL)], content, at)
}

#[test]
fn help_lists_the_five_commands() {
    let output = binary(UNREACHABLE)
        .arg("help")
        .output()
        .expect("the buzzx binary");
    assert!(output.status.success(), "help exits 0");
    let text = String::from_utf8_lossy(&output.stdout);
    for command in ["channels", "messages", "tui", "watch"] {
        assert!(text.contains(command), "help names {command}: {text}");
    }
    // Acceptance criterion 11: the history help separates which messages are
    // selected from the order they come back in.
    let output = binary(UNREACHABLE)
        .args(["messages", "get", "--help"])
        .output()
        .expect("the buzzx binary");
    let get_help = String::from_utf8_lossy(&output.stdout);
    assert!(
        get_help.contains("latest") && get_help.contains("oldest first"),
        "the history help names the selection and the order: {get_help}"
    );
    // A usage error is bad input: code 1, not clap's own 2, and it stays on
    // stderr as prose.
    for args in [vec!["--nope"], vec!["messages", "nope"], vec!["channels"]] {
        let output = binary(UNREACHABLE)
            .args(&args)
            .output()
            .expect("the buzzx binary");
        assert_eq!(output.status.code(), Some(1), "usage error {args:?}");
        assert!(
            output.stdout.is_empty(),
            "usage error {args:?} prints no JSON"
        );
        assert!(
            !output.stderr.is_empty(),
            "usage error {args:?} explains itself on stderr"
        );
    }
}

#[test]
fn channels_list_returns_one_object_per_channel() {
    let relay = FakeRelay::start();
    let keys = keys();
    // The roster and the metadata are the relay's own events, and a roster
    // entry names the member. An event cannot tag its own author, so the
    // signer and the named member are different identities here.
    let relay_keys = Keys::generate();
    let named = CHANNEL;
    let unnamed = OTHER_CHANNEL;
    relay.seed(&event(
        &relay_keys,
        39002,
        vec![
            d_tag(named),
            Tag::parse(["p", keys.public_key().to_hex().as_str()]).unwrap(),
        ],
        "",
        10,
    ));
    relay.seed(&event(
        &relay_keys,
        39002,
        vec![
            d_tag(unnamed),
            Tag::parse(["p", keys.public_key().to_hex().as_str()]).unwrap(),
        ],
        "",
        11,
    ));
    relay.seed(&event(
        &relay_keys,
        39000,
        vec![d_tag(named), Tag::parse(["name", "buzzx-cli"]).unwrap()],
        "",
        12,
    ));

    let (code, json, stderr) = run(&relay.url, &keys, &["channels", "list"], None);
    assert_eq!(code, 0, "stderr: {stderr}");
    // Sorted by lowercase name: the id-as-name entry sorts before "buzzx-cli".
    assert_eq!(
        json,
        json!([
            {"channel_id": unnamed, "name": unnamed, "community": {"id": null, "name": "default", "relay_url": relay.url}},
            {"channel_id": named, "name": "buzzx-cli", "community": {"id": null, "name": "default", "relay_url": relay.url}},
        ])
    );
}

#[test]
fn messages_get_returns_the_newest_limit_oldest_first() {
    let relay = FakeRelay::start();
    let keys = keys();
    let (first, second, third) = (
        message(&keys, "one", 100),
        message(&keys, "two", 200),
        message(&keys, "three", 300),
    );
    relay.seed(&first);
    relay.seed(&second);
    relay.seed(&third);
    // Another channel's message must not appear.
    relay.seed(&event(
        &keys,
        9,
        vec![h_tag(OTHER_CHANNEL)],
        "elsewhere",
        400,
    ));

    let (code, json, stderr) = run(
        &relay.url,
        &keys,
        &["messages", "get", "--channel", CHANNEL, "--limit", "2"],
        None,
    );
    assert_eq!(code, 0, "stderr: {stderr}");
    let rows = json.as_array().expect("an array");
    assert_eq!(rows.len(), 2, "{json}");
    assert_eq!(rows[0]["content"], json!("two"));
    assert_eq!(rows[1]["content"], json!("three"));
    // Every field of the contract is present, with null where unknown.
    assert_eq!(
        rows[1],
        json!({
            "channel_id": CHANNEL,
            "event_id": third.id.to_hex(),
            "author": keys.public_key().to_hex(),
            "created_at": 300,
            "thread_root": null,
            "reply_to": null,
            "content": "three",
            "community": {"id": null, "name": "default", "relay_url": relay.url},
        })
    );
}

#[test]
fn messages_get_event_resolves_one_event_and_not_found_names_it() {
    let relay = FakeRelay::start();
    let keys = keys();
    let only = message(&keys, "hello", 100);
    relay.seed(&only);

    let (code, json, stderr) = run(
        &relay.url,
        &keys,
        &["messages", "get", "--event", &only.id.to_hex()],
        None,
    );
    assert_eq!(code, 0, "stderr: {stderr}");
    assert_eq!(json["event_id"], json!(only.id.to_hex()));
    assert_eq!(json["content"], json!("hello"));

    let missing = "cd".repeat(32);
    let (code, json, stderr) = run(
        &relay.url,
        &keys,
        &["messages", "get", "--event", &missing],
        None,
    );
    assert_eq!(code, 1, "stderr: {stderr}");
    assert_eq!(json["error"], json!("not_found"));
    assert_eq!(json["event_id"], json!(missing));
}

#[test]
fn messages_thread_returns_the_root_first() {
    let relay = FakeRelay::start();
    let keys = keys();
    let root = message(&keys, "root", 100);
    let direct = event(
        &keys,
        9,
        vec![
            h_tag(CHANNEL),
            Tag::parse(["e", &root.id.to_hex(), "", "reply"]).unwrap(),
        ],
        "direct reply",
        200,
    );
    let nested = event(
        &keys,
        9,
        vec![
            h_tag(CHANNEL),
            Tag::parse(["e", &root.id.to_hex(), "", "root"]).unwrap(),
            Tag::parse(["e", &direct.id.to_hex(), "", "reply"]).unwrap(),
        ],
        "nested reply",
        300,
    );
    relay.seed(&nested);
    relay.seed(&root);
    relay.seed(&direct);

    let (code, json, stderr) = run(
        &relay.url,
        &keys,
        &["messages", "thread", "--event", &root.id.to_hex()],
        None,
    );
    assert_eq!(code, 0, "stderr: {stderr}");
    let rows = json.as_array().expect("an array");
    let ids: Vec<&str> = rows
        .iter()
        .map(|row| row["event_id"].as_str().unwrap())
        .collect();
    assert_eq!(
        ids,
        vec![
            root.id.to_hex().as_str(),
            direct.id.to_hex().as_str(),
            nested.id.to_hex().as_str()
        ]
    );
    // The nested reply's fields come back through the same projection.
    assert_eq!(rows[2]["thread_root"], json!(root.id.to_hex()));
    assert_eq!(rows[2]["reply_to"], json!(direct.id.to_hex()));
    assert_eq!(rows[0]["thread_root"], Value::Null);
}

#[test]
fn messages_send_confirms_with_the_relays_id_and_keeps_stdin_bytes() {
    let relay = FakeRelay::start();
    let keys = keys();
    let content = "first line\n\nsecond line\n";
    let (code, json, stderr) = run(
        &relay.url,
        &keys,
        &["messages", "send", "--channel", CHANNEL, "--content", "-"],
        Some(content),
    );
    assert_eq!(code, 0, "stderr: {stderr}");
    assert_eq!(json["status"], json!("sent_confirmed"));
    assert_eq!(json["channel_id"], json!(CHANNEL));

    let writes = relay.writes();
    assert_eq!(writes.len(), 1, "one write, one event");
    let sent = &writes[0];
    assert_eq!(json["event_id"], sent["id"], "the relay echoes its id");
    assert_eq!(sent["content"], json!(content), "stdin bytes are kept");
    assert_eq!(sent["kind"], json!(9));
    assert!(
        sent["tags"]
            .as_array()
            .unwrap()
            .iter()
            .any(|tag| tag == &json!(["h", CHANNEL])),
        "the message is channel-scoped: {sent}"
    );
}

#[test]
fn messages_send_returns_the_canonical_id_the_relay_reports() {
    let relay = FakeRelay::start();
    relay.answer(WriteAnswer::Recanonicalized);
    let keys = keys();
    let (code, json, stderr) = run(
        &relay.url,
        &keys,
        &[
            "messages",
            "send",
            "--channel",
            CHANNEL,
            "--content",
            "hello",
        ],
        None,
    );
    assert_eq!(code, 0, "stderr: {stderr}");
    let canonical = "ab".repeat(32);
    assert_eq!(json["status"], json!("sent_confirmed"));
    assert_eq!(json["event_id"], json!(canonical));
    assert_ne!(
        json["event_id"],
        relay.writes()[0]["id"],
        "the relay's id, not the signed one, is what a caller addresses"
    );
}

#[test]
fn messages_reply_derives_the_channel_and_the_thread_from_the_target() {
    let relay = FakeRelay::start();
    let keys = keys();
    let root = message(&keys, "root", 100);
    relay.seed(&root);

    let (code, json, stderr) = run(
        &relay.url,
        &keys,
        &[
            "messages",
            "reply",
            "--event",
            &root.id.to_hex(),
            "--content",
            "hi",
        ],
        None,
    );
    assert_eq!(code, 0, "stderr: {stderr}");
    assert_eq!(json["status"], json!("sent_confirmed"));
    assert_eq!(json["reply_to"], json!(root.id.to_hex()));
    assert_eq!(json["channel_id"], json!(CHANNEL));

    let writes = relay.writes();
    let sent = &writes[0];
    assert!(
        sent["tags"]
            .as_array()
            .unwrap()
            .iter()
            .any(|tag| tag == &json!(["h", CHANNEL])),
        "the channel comes from the target: {sent}"
    );
    assert!(
        sent["tags"]
            .as_array()
            .unwrap()
            .iter()
            .any(|tag| tag == &json!(["e", root.id.to_hex(), "", "reply"])),
        "the reply names the target it answers: {sent}"
    );
}

#[test]
fn a_reply_to_an_unresolved_event_is_not_sent_and_writes_nothing() {
    let relay = FakeRelay::start();
    let keys = keys();
    let missing = "ef".repeat(32);
    let (code, json, stderr) = run(
        &relay.url,
        &keys,
        &["messages", "reply", "--event", &missing, "--content", "hi"],
        None,
    );
    assert_eq!(code, 1, "stderr: {stderr}");
    // The reply was never submitted, and the event id of the reply that was
    // not stored stays null: the target is reply context, not a new event.
    assert_eq!(json["status"], json!("not_sent"));
    assert_eq!(json["event_id"], Value::Null);
    assert_eq!(json["reply_to"], json!(missing));
    assert_eq!(json["error"], json!("not_found"));
    assert!(json.get("channel_id").is_none(), "{json}");
    assert!(relay.writes().is_empty(), "nothing was signed or sent");
}

#[test]
fn a_refused_write_is_not_sent() {
    let relay = FakeRelay::start();
    relay.answer(WriteAnswer::Refused);
    let keys = keys();
    let (code, json, stderr) = run(
        &relay.url,
        &keys,
        &[
            "messages",
            "send",
            "--channel",
            CHANNEL,
            "--content",
            "hello",
        ],
        None,
    );
    assert_eq!(code, 3, "stderr: {stderr}");
    assert_eq!(json["status"], json!("not_sent"));
    assert_eq!(json["error"], json!("forbidden"));
    assert_eq!(json["message"], json!("HTTP 403 Forbidden: not a member"));
    assert_eq!(json["event_id"], Value::Null);
    assert_eq!(relay.writes().len(), 1, "a refusal is not retried");
}

#[test]
fn a_lost_answer_is_unconfirmed_and_never_retried() {
    let relay = FakeRelay::start();
    relay.answer(WriteAnswer::Lost);
    let keys = keys();
    let (code, json, stderr) = run(
        &relay.url,
        &keys,
        &[
            "messages",
            "send",
            "--channel",
            CHANNEL,
            "--content",
            "hello",
        ],
        None,
    );
    assert_eq!(code, 2, "stderr: {stderr}");
    assert_eq!(json["status"], json!("sent_unconfirmed"));
    assert_eq!(json["error"], json!("timeout_unknown"));
    assert_eq!(json["event_id"], Value::Null);
    assert_eq!(
        relay.writes().len(),
        1,
        "an unconfirmed write is never submitted twice"
    );
}

#[test]
fn a_lost_read_answer_is_retried_once() {
    let relay = FakeRelay::start();
    let keys = keys();
    relay.seed(&message(&keys, "hello", 100));
    relay.drop_reads(1);
    let (code, json, stderr) = run(
        &relay.url,
        &keys,
        &["messages", "get", "--channel", CHANNEL],
        None,
    );
    assert_eq!(code, 0, "stderr: {stderr}");
    assert_eq!(json[0]["content"], json!("hello"));
    assert_eq!(relay.queries(), 2, "a read is repeated once: {json}");
}

#[test]
fn an_unreachable_relay_is_network_and_a_write_stores_nothing() {
    let keys = keys();
    let dead = "http://127.0.0.1:1";
    let (code, json, stderr) = run(dead, &keys, &["channels", "list"], None);
    assert_eq!(code, 2, "stderr: {stderr}");
    assert_eq!(json["error"], json!("network"));

    let (code, json, stderr) = run(
        dead,
        &keys,
        &[
            "messages",
            "send",
            "--channel",
            CHANNEL,
            "--content",
            "hello",
        ],
        None,
    );
    assert_eq!(code, 2, "stderr: {stderr}");
    assert_eq!(json["status"], json!("not_sent"));
    assert_eq!(json["error"], json!("network"));
}

#[test]
fn bad_input_is_rejected_before_the_relay_is_touched() {
    /// One rejected invocation: its arguments, optional stdin, the category
    /// it must name, and the `status` a recognized write must carry.
    struct Rejected<'a> {
        args: &'a [&'a str],
        stdin: Option<&'a str>,
        category: &'a str,
        status: Option<&'a str>,
    }

    let relay = FakeRelay::start();
    let keys = keys();
    let cases = [
        Rejected {
            args: &[
                "messages",
                "get",
                "--channel",
                CHANNEL,
                "--event",
                &"cd".repeat(32),
            ],
            stdin: None,
            category: "invalid_input",
            status: None,
        },
        Rejected {
            args: &["messages", "get"],
            stdin: None,
            category: "invalid_input",
            status: None,
        },
        Rejected {
            args: &[
                "messages",
                "get",
                "--event",
                &"cd".repeat(32),
                "--limit",
                "5",
            ],
            stdin: None,
            category: "invalid_input",
            status: None,
        },
        Rejected {
            args: &["messages", "send", "--channel", CHANNEL],
            stdin: None,
            category: "invalid_input",
            status: Some("not_sent"),
        },
        Rejected {
            args: &["messages", "send", "--content", "hi"],
            stdin: None,
            category: "invalid_input",
            status: Some("not_sent"),
        },
        Rejected {
            args: &["messages", "send", "--channel", "nope", "--content", "hi"],
            stdin: None,
            category: "invalid_input",
            status: Some("not_sent"),
        },
        Rejected {
            args: &["messages", "send", "--channel", CHANNEL, "--content", "-"],
            stdin: Some(""),
            category: "invalid_input",
            status: Some("not_sent"),
        },
        Rejected {
            args: &["messages", "reply", "--content", "hi"],
            stdin: None,
            category: "invalid_input",
            status: Some("not_sent"),
        },
        Rejected {
            args: &["messages", "reply", "--event", "nope", "--content", "hi"],
            stdin: None,
            category: "invalid_input",
            status: Some("not_sent"),
        },
        Rejected {
            args: &["messages", "reply", "--event", &"cd".repeat(32)],
            stdin: None,
            category: "invalid_input",
            status: Some("not_sent"),
        },
    ];
    for case in cases {
        let (code, json, stderr) = run(&relay.url, &keys, case.args, case.stdin);
        let args = case.args;
        assert_eq!(code, 1, "{args:?} stderr: {stderr}");
        assert_eq!(json["error"], json!(case.category), "{args:?}: {json}");
        assert!(json["message"].is_string(), "{args:?}: {json}");
        match case.status {
            Some(status) => {
                assert_eq!(json["status"], json!(status), "{args:?}: {json}");
                assert_eq!(json["event_id"], Value::Null, "{args:?}: {json}");
            }
            None => assert!(json.get("status").is_none(), "{args:?}: {json}"),
        }
    }
    assert!(
        relay.writes().is_empty(),
        "bad input never reaches the relay"
    );
    assert_eq!(
        relay.queries(),
        0,
        "bad input is rejected before the first read"
    );
}

#[test]
fn a_failed_write_names_the_ids_the_caller_gave() {
    let relay = FakeRelay::start();
    let keys = keys();
    // A send that never resolved its channel names the one it was given.
    let (_, json, _) = run(
        &relay.url,
        &keys,
        &["messages", "send", "--channel", "nope", "--content", "hi"],
        None,
    );
    assert_eq!(json["channel_id"], json!("nope"));
    assert!(json.get("reply_to").is_none(), "{json}");

    // A reply names its target in `reply_to` and never in `event_id`.
    let target = "ab".repeat(32);
    let (_, json, _) = run(
        &relay.url,
        &keys,
        &["messages", "reply", "--event", &target, "--content", "hi"],
        None,
    );
    assert_eq!(json["reply_to"], json!(target));
    assert_eq!(json["event_id"], Value::Null);
    assert!(json.get("channel_id").is_none(), "{json}");

    // A reply to an event that is not channel-scoped resolves but cannot
    // route: it is bad input, with the target still named.
    let relay_event = event(&Keys::generate(), 1, Vec::new(), "", 10);
    relay.seed(&relay_event);
    let (code, json, _) = run(
        &relay.url,
        &keys,
        &[
            "messages",
            "reply",
            "--event",
            &relay_event.id.to_hex(),
            "--content",
            "hi",
        ],
        None,
    );
    assert_eq!(code, 1);
    assert_eq!(json["status"], json!("not_sent"));
    assert_eq!(json["error"], json!("invalid_input"));
    assert_eq!(json["reply_to"], json!(relay_event.id.to_hex()));
    assert!(relay.writes().is_empty(), "nothing was signed or sent");
}

#[test]
fn a_malformed_channel_is_bad_input_and_names_the_id() {
    let relay = FakeRelay::start();
    let keys = keys();
    let (code, json, stderr) = run(
        &relay.url,
        &keys,
        &["messages", "get", "--channel", "not-a-uuid"],
        None,
    );
    assert_eq!(code, 1, "stderr: {stderr}");
    assert_eq!(json["error"], json!("invalid_input"));
    assert_eq!(json["channel_id"], json!("not-a-uuid"));
}

#[test]
fn a_write_refused_without_naming_the_identity_exits_four() {
    let relay = FakeRelay::start();
    relay.answer(WriteAnswer::RefusedBadRequest);
    let keys = keys();
    let (code, json, stderr) = run(
        &relay.url,
        &keys,
        &[
            "messages",
            "send",
            "--channel",
            CHANNEL,
            "--content",
            "hello",
        ],
        None,
    );
    assert_eq!(code, 4, "stderr: {stderr}");
    assert_eq!(json["status"], json!("not_sent"));
    assert_eq!(json["error"], json!("relay_rejected"));
    assert_eq!(json["event_id"], Value::Null);
    assert_eq!(relay.writes().len(), 1, "a refusal is not retried");
}

#[test]
fn a_relay_failure_after_a_write_exits_by_its_category() {
    let relay = FakeRelay::start();
    relay.answer(WriteAnswer::ServerError);
    let keys = keys();
    let (code, json, stderr) = run(
        &relay.url,
        &keys,
        &[
            "messages",
            "send",
            "--channel",
            CHANNEL,
            "--content",
            "hello",
        ],
        None,
    );
    // The relay answered, and storage is not established: the write is
    // unconfirmed, and the exit code is the one its category carries, so a
    // script never reads it as a lost answer.
    assert_eq!(code, 4, "stderr: {stderr}");
    assert_eq!(json["status"], json!("sent_unconfirmed"));
    assert_eq!(json["error"], json!("relay_rejected"));
    assert_eq!(json["event_id"], Value::Null);
    assert_eq!(relay.writes().len(), 1, "a failed answer is not retried");
}

#[test]
fn a_relay_failure_after_a_reply_keeps_the_reply_ids() {
    let relay = FakeRelay::start();
    let keys = keys();
    let root = message(&keys, "root", 100);
    relay.seed(&root);
    relay.answer(WriteAnswer::ServerError);
    let (code, json, stderr) = run(
        &relay.url,
        &keys,
        &[
            "messages",
            "reply",
            "--event",
            &root.id.to_hex(),
            "--content",
            "hi",
        ],
        None,
    );
    // The lookup succeeded, so the failure is the write's: the reply carries
    // the channel it derived and the target it answers, and no event id,
    // because storage is not established.
    assert_eq!(code, 4, "stderr: {stderr}");
    assert_eq!(json["status"], json!("sent_unconfirmed"));
    assert_eq!(json["error"], json!("relay_rejected"));
    assert_eq!(json["event_id"], Value::Null);
    assert_eq!(json["channel_id"], json!(CHANNEL));
    assert_eq!(json["reply_to"], json!(root.id.to_hex()));
    assert_eq!(relay.writes().len(), 1, "a failed answer is not retried");
}

#[test]
fn an_unreadable_relay_answer_is_not_an_empty_read() {
    let relay = FakeRelay::start();
    let keys = keys();
    let id = "ef".repeat(32);
    // The relay holds a row for the id, but the row is not an event.
    relay.seed_unreadable(json!({"id": id, "kind": 9}));
    let (code, json, stderr) = run(
        &relay.url,
        &keys,
        &["messages", "get", "--event", &id],
        None,
    );
    assert_eq!(code, 4, "stderr: {stderr}");
    assert_eq!(json["error"], json!("relay_rejected"));
}

#[test]
fn a_startup_failure_prints_one_json_error() {
    // A failure raised before the command ran answers in the recognized
    // command's shape: one error object for a read, and a write that was
    // never sent for a write. The category and the exit code are the
    // reason's, so a script sees the code the category table promises.
    let valid_key = keys().secret_key().to_secret_hex();
    let reasons = [
        (None, UNREACHABLE, 3, "forbidden", "identity"),
        (
            Some("not-a-key"),
            UNREACHABLE,
            3,
            "forbidden",
            "private key",
        ),
        (
            Some(valid_key.as_str()),
            "not-a-url",
            1,
            "invalid_input",
            "has no scheme",
        ),
    ];
    for (key, relay, code, category, reason) in reasons {
        for (args, channel, reply_to) in [
            (vec!["channels", "list"], None, None),
            (vec!["messages", "get", "--channel", CHANNEL], None, None),
            (
                vec!["messages", "send", "--channel", CHANNEL, "--content", "hi"],
                Some(CHANNEL),
                None,
            ),
            (
                vec![
                    "messages",
                    "reply",
                    "--event",
                    &"cd".repeat(32),
                    "--content",
                    "hi",
                ],
                None,
                Some("cd".repeat(32)),
            ),
        ] {
            let mut command = binary(relay);
            command.args(&args);
            match key {
                Some(key) => command.env("BUZZ_PRIVATE_KEY", key),
                None => command.env_remove("BUZZ_PRIVATE_KEY"),
            };
            let output = command.output().expect("the buzzx binary");
            let how = format!("{args:?} {relay} key={key:?}");
            assert_eq!(
                output.status.code(),
                Some(code),
                "{how}: the code the category carries"
            );
            let json: Value =
                serde_json::from_slice(&output.stdout).expect("one JSON object on stdout");
            assert_eq!(json["error"], json!(category), "{how}: {json}");
            assert!(
                json["message"]
                    .as_str()
                    .is_some_and(|message| message.contains(reason)),
                "{how}: {json}"
            );
            assert!(!output.stderr.is_empty(), "{how}: the reason is on stderr");
            match (&channel, &reply_to) {
                (None, None) => assert!(json.get("status").is_none(), "{how}: {json}"),
                _ => {
                    assert_eq!(json["status"], json!("not_sent"), "{how}: {json}");
                    assert_eq!(json["event_id"], Value::Null, "{how}: {json}");
                    if let Some(channel) = channel {
                        assert_eq!(json["channel_id"], json!(channel), "{how}: {json}");
                    }
                    if let Some(reply_to) = &reply_to {
                        assert_eq!(json["reply_to"], json!(reply_to), "{how}: {json}");
                    }
                }
            }
        }
    }
}

/// Seed a channel roster: the kind 39002 event names each member with its
/// role, and each member's kind 0 event carries the display name the mention
/// preflight matches on.
fn seed_roster(relay: &FakeRelay, channel: &str, members: &[(&Keys, &str, &str)]) {
    let relay_keys = Keys::generate();
    let mut tags = vec![d_tag(channel)];
    for (member, role, _) in members {
        tags.push(
            Tag::parse(["p", member.public_key().to_hex().as_str(), "", role])
                .expect("a member tag"),
        );
    }
    relay.seed(&event(&relay_keys, 39002, tags, "", 50));
    for (member, _, name) in members {
        relay.seed(&event(
            member,
            0,
            vec![],
            &format!("{{\"display_name\":\"{name}\"}}"),
            51,
        ));
    }
}

fn carries_tag(event: &Value, name: &str, value: &str) -> bool {
    event["tags"].as_array().is_some_and(|tags| {
        tags.iter().any(|tag| {
            tag.get(0).and_then(Value::as_str) == Some(name)
                && tag.get(1).and_then(Value::as_str) == Some(value)
        })
    })
}

#[test]
fn a_cli_send_signs_the_member_a_visible_name_names() {
    let relay = FakeRelay::start();
    let me = keys();
    let member = keys();
    seed_roster(&relay, CHANNEL, &[(&member, "member", "Buzzx Build")]);

    let (code, json, stderr) = run(
        &relay.url,
        &me,
        &[
            "messages",
            "send",
            "--channel",
            CHANNEL,
            "--content",
            "@Buzzx Build please look",
        ],
        None,
    );
    assert_eq!(code, 0, "stderr: {stderr}");
    assert_eq!(json["status"], json!("sent_confirmed"));
    let named = member.public_key().to_hex();
    assert_eq!(json["mention_pubkeys"], json!([named]));

    let writes = relay.writes();
    assert_eq!(writes.len(), 1, "one write, one event");
    let sent = &writes[0];
    assert_eq!(
        sent["content"],
        json!("@Buzzx Build please look"),
        "the body is stored exactly as typed"
    );
    assert!(
        carries_tag(sent, "p", &named),
        "the visible name becomes a signed recipient: {sent}"
    );
}

#[test]
fn an_unknown_name_blocks_a_cli_send_without_a_write() {
    let relay = FakeRelay::start();
    let me = keys();
    let member = keys();
    seed_roster(&relay, CHANNEL, &[(&member, "member", "Buzzx Build")]);

    let (code, json, stderr) = run(
        &relay.url,
        &me,
        &[
            "messages",
            "send",
            "--channel",
            CHANNEL,
            "--content",
            "@Nobody please look",
        ],
        None,
    );
    assert_eq!(code, 1, "a blocked draft is bad input: {stderr}");
    assert_eq!(json["status"], json!("not_sent"));
    assert_eq!(json["event_id"], Value::Null);
    assert_eq!(json["channel_id"], json!(CHANNEL));
    assert_eq!(json["mention_block"]["kind"], json!("unknown"));
    assert!(
        json["mention_block"]["summary"]
            .as_str()
            .unwrap()
            .contains("@nobody"),
        "the refusal names the fragment: {json}"
    );
    assert!(
        relay.writes().is_empty(),
        "a draft that cannot be resolved is never published"
    );
}

#[test]
fn an_ambiguous_name_blocks_with_exact_references() {
    let relay = FakeRelay::start();
    let me = keys();
    let first = keys();
    let second = keys();
    seed_roster(
        &relay,
        CHANNEL,
        &[
            (&first, "member", "Buzzx Build"),
            (&second, "bot", "Buzzx Build"),
        ],
    );

    let (code, json, stderr) = run(
        &relay.url,
        &me,
        &[
            "messages",
            "send",
            "--channel",
            CHANNEL,
            "--content",
            "@Buzzx Build ping",
        ],
        None,
    );
    assert_eq!(code, 1, "an ambiguous name is bad input: {stderr}");
    assert_eq!(json["status"], json!("not_sent"));
    assert_eq!(json["mention_block"]["kind"], json!("ambiguous"));
    let details = json["mention_block"]["details"].as_array().unwrap();
    assert_eq!(details.len(), 2, "both candidates are named: {json}");
    for detail in details {
        assert!(
            detail
                .as_str()
                .unwrap()
                .starts_with("@buzzx build -> nostr:npub1"),
            "a correction is a complete exact reference: {detail}"
        );
    }
    assert!(relay.writes().is_empty());
}

#[test]
fn a_cli_reply_carries_the_recipients_with_the_thread_tags() {
    let relay = FakeRelay::start();
    let me = keys();
    let member = keys();
    let root = message(&me, "root", 100);
    relay.seed(&root);
    seed_roster(&relay, CHANNEL, &[(&member, "member", "Buzzx Product")]);

    let (code, json, stderr) = run(
        &relay.url,
        &me,
        &[
            "messages",
            "reply",
            "--event",
            &root.id.to_hex(),
            "--content",
            "@Buzzx Product done",
        ],
        None,
    );
    assert_eq!(code, 0, "stderr: {stderr}");
    assert_eq!(json["status"], json!("sent_confirmed"));
    assert_eq!(json["reply_to"], json!(root.id.to_hex()));
    assert_eq!(
        json["mention_pubkeys"],
        json!([member.public_key().to_hex()])
    );

    let writes = relay.writes();
    let sent = &writes[0];
    assert!(
        carries_tag(sent, "e", &root.id.to_hex()),
        "the reply keeps its thread context: {sent}"
    );
    assert!(
        carries_tag(sent, "p", &member.public_key().to_hex()),
        "the reply notifies the member it names: {sent}"
    );
}

#[test]
fn a_failed_member_read_blocks_a_cli_send() {
    let relay = FakeRelay::start();
    let me = keys();
    // Two dropped answers: the read is retried once, and both are lost.
    relay.drop_reads(2);

    let (code, json, stderr) = run(
        &relay.url,
        &me,
        &[
            "messages",
            "send",
            "--channel",
            CHANNEL,
            "--content",
            "@Buzzx Build look",
        ],
        None,
    );
    assert_eq!(
        code, 1,
        "an unreadable roster is not an empty one: {stderr}"
    );
    assert_eq!(json["status"], json!("not_sent"));
    assert_eq!(json["mention_block"]["kind"], json!("directory_failed"));
    assert!(
        relay.writes().is_empty(),
        "the draft is not published against a guess"
    );
}

#[test]
fn channels_create_reports_the_confirmed_channel_and_its_fields() {
    let relay = FakeRelay::start();
    let me = keys();
    let (code, json, stderr) = run(
        &relay.url,
        &me,
        &[
            "channels",
            "create",
            "--name",
            "  项目讨论  ",
            "--type",
            "forum",
            "--visibility",
            "private",
            "--description",
            "可选说明",
        ],
        None,
    );
    assert_eq!(code, 0, "stderr: {stderr}");
    assert_eq!(json["status"], json!("created_confirmed"));
    assert_eq!(json["name"], json!("项目讨论"), "the name is trimmed");
    assert_eq!(json["type"], json!("forum"));
    assert_eq!(json["visibility"], json!("private"));
    assert_eq!(json["description"], json!("可选说明"));
    let channel = json["channel_id"].as_str().expect("a channel id");
    assert!(uuid::Uuid::parse_str(channel).is_ok(), "{channel}");

    let writes = relay.writes();
    assert_eq!(writes.len(), 1, "one write, one channel event");
    let sent = &writes[0];
    assert_eq!(sent["kind"], json!(9007), "the create-group event: {sent}");
    for (name, value) in [
        ("h", channel),
        ("name", "项目讨论"),
        ("channel_type", "forum"),
        ("visibility", "private"),
        ("about", "可选说明"),
    ] {
        assert!(
            carries_tag(sent, name, value),
            "the event carries {name}={value}: {sent}"
        );
    }
}

#[test]
fn a_lost_channel_creation_is_unconfirmed_and_written_once() {
    let relay = FakeRelay::start();
    relay.answer(WriteAnswer::Lost);
    let me = keys();
    let (code, json, stderr) = run(
        &relay.url,
        &me,
        &["channels", "create", "--name", "standup"],
        None,
    );
    assert_eq!(code, 2, "a lost answer is a timeout: {stderr}");
    assert_eq!(json["status"], json!("created_unconfirmed"));
    assert_eq!(json["error"], json!("timeout_unknown"));
    assert!(json["channel_id"].as_str().is_some());
    assert_eq!(
        relay.writes().len(),
        1,
        "an unconfirmed creation is never submitted twice"
    );
}

#[test]
fn a_refused_channel_creation_is_not_created() {
    let relay = FakeRelay::start();
    relay.answer(WriteAnswer::Refused);
    let me = keys();
    let (code, json, stderr) = run(
        &relay.url,
        &me,
        &["channels", "create", "--name", "standup"],
        None,
    );
    assert_eq!(code, 3, "a refusal names the identity: {stderr}");
    assert_eq!(json["status"], json!("not_created"));
    assert_eq!(json["error"], json!("forbidden"));
    assert_eq!(json["channel_id"], Value::Null);
    assert_eq!(relay.writes().len(), 1, "the refusal was one attempt");
}

#[test]
fn channels_create_rejects_invalid_fields_before_the_relay() {
    let relay = FakeRelay::start();
    let me = keys();
    for (args, reason) in [
        (vec!["channels", "create"], "name is required"),
        (
            vec!["channels", "create", "--name", "standup", "--type", "blog"],
            "channel type",
        ),
        (
            vec!["channels", "create", "--name", "standup", "--type", "dm"],
            "channel type",
        ),
        (
            vec![
                "channels", "create", "--name", "standup", "--type", "workflow",
            ],
            "channel type",
        ),
        (
            vec![
                "channels",
                "create",
                "--name",
                "standup",
                "--visibility",
                "secret",
            ],
            "channel visibility",
        ),
        (
            vec!["channels", "create", "--name", "   "],
            "name is required",
        ),
    ] {
        let (code, json, stderr) = run(&relay.url, &me, &args, None);
        assert_eq!(code, 1, "{args:?} is bad input: {stderr}");
        assert_eq!(json["status"], json!("not_created"), "{args:?}: {json}");
        assert_eq!(json["error"], json!("invalid_input"), "{args:?}: {json}");
        assert!(
            json["message"].as_str().unwrap().contains(reason),
            "{args:?} explains itself: {json}"
        );
        assert_eq!(json["channel_id"], Value::Null);
        // The refusal echoes what the caller sent, so a script sees the field
        // the relay-never-saw attempt carried.
        assert_eq!(
            json["name"],
            json!(
                args.iter()
                    .position(|arg| *arg == "--name")
                    .map(|at| args[at + 1])
            ),
            "{args:?}: {json}"
        );
        assert_eq!(
            json["type"],
            json!(
                args.iter()
                    .position(|arg| *arg == "--type")
                    .map(|at| args[at + 1])
            ),
            "{args:?}: {json}"
        );
        assert_eq!(
            json["visibility"],
            json!(
                args.iter()
                    .position(|arg| *arg == "--visibility")
                    .map(|at| args[at + 1])
            ),
            "{args:?}: {json}"
        );
    }
    assert!(relay.writes().is_empty(), "nothing reached the relay");
    assert_eq!(relay.queries(), 0, "no channel event was even prepared");
}
