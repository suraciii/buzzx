//! The five one-shot commands against a local fake relay: the JSON on stdout
//! and the exit codes. The product contract is docs/browse-collab-cli.md.
//!
//! The fake relay answers `/query` by applying the filter to the events the
//! test seeded, and `/events` by recording the body and answering with the
//! status the test chose. Every event is signed for real, and the NIP-98
//! header of a write is checked against the body it authorizes.

use std::io::{BufRead, BufReader, Read, Write};
use std::net::{TcpListener, TcpStream};
use std::process::{Command, Stdio};
use std::sync::Arc;
use std::thread;

use nostr::{Event, EventBuilder, Keys, Kind, Tag};
use parking_lot::Mutex;
use serde_json::{Value, json};

/// The channel every seeded event belongs to.
const CHANNEL: &str = "7e5faaba-948a-47b5-8ca0-20c6e47953d3";
/// A second channel, to prove a filter does not leak between them.
const OTHER_CHANNEL: &str = "2c1e6c1a-1a2b-4f3a-9c1d-0f1e2d3c4b5a";

/// What the fake relay does with a write.
#[derive(Clone, Copy, PartialEq)]
enum WriteAnswer {
    /// Store it and answer with the relay's canonical id.
    Stored,
    /// Store it and answer with a different id than the signed one.
    Recanonicalized,
    /// Refuse it: nothing was stored.
    Refused,
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
        thread::spawn(move || {
            for stream in listener.incoming() {
                match stream {
                    Ok(stream) => {
                        let state = Arc::clone(&served);
                        thread::spawn(move || serve(stream, &state));
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

fn serve(stream: TcpStream, state: &Arc<Mutex<State>>) {
    let mut reader = BufReader::new(stream.try_clone().unwrap());
    let mut request_line = String::new();
    if reader.read_line(&mut request_line).is_err() {
        return;
    }
    let path = request_line
        .split_whitespace()
        .nth(1)
        .unwrap_or("")
        .to_owned();
    let mut length = 0usize;
    loop {
        let mut line = String::new();
        if reader.read_line(&mut line).unwrap_or(0) == 0 || line.trim().is_empty() {
            break;
        }
        if let Some(value) = line.to_lowercase().strip_prefix("content-length:") {
            length = value.trim().parse().unwrap_or(0);
        }
    }
    let mut body = vec![0u8; length];
    let _ = reader.read_exact(&mut body);
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
            match state.answer {
                WriteAnswer::Stored => (200, json!({"event_id": request["id"]})),
                WriteAnswer::Recanonicalized => (200, json!({"event_id": "ab".repeat(32)})),
                WriteAnswer::Refused => (403, json!({"error": "not a member"})),
                WriteAnswer::Lost => (502, json!({"error": "upstream unreachable"})),
            }
        }
        _ => (404, json!({"error": "no such path"})),
    };
    let body = answer.to_string();
    let response = format!(
        "HTTP/1.1 {status} OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
        body.len()
    );
    let mut stream = stream;
    let _ = stream.write_all(response.as_bytes());
    let _ = stream.flush();
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

/// Run one command against the relay. Returns the exit code, stdout as JSON,
/// and stderr.
fn run(relay: &str, keys: &Keys, args: &[&str], stdin: Option<&str>) -> (i32, Value, String) {
    let mut command = Command::new(env!("CARGO_BIN_EXE_buzzx"));
    command
        .args(args)
        .env("BUZZ_RELAY_URL", relay)
        .env("BUZZ_PRIVATE_KEY", keys.secret_key().to_secret_hex())
        .env("BUZZX_CONFIG", "/nonexistent/buzzx-test-config")
        .env_remove("BUZZ_AUTH_TAG")
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
    let output = Command::new(env!("CARGO_BIN_EXE_buzzx"))
        .arg("help")
        .output()
        .expect("the buzzx binary");
    assert!(output.status.success(), "help exits 0");
    let text = String::from_utf8_lossy(&output.stdout);
    for command in ["channels", "messages", "tui", "watch"] {
        assert!(text.contains(command), "help names {command}: {text}");
    }
    // A usage error is bad input: code 1, not clap's own 2, and it stays on
    // stderr as prose.
    for args in [vec!["--nope"], vec!["messages", "nope"], vec!["channels"]] {
        let output = Command::new(env!("CARGO_BIN_EXE_buzzx"))
            .args(&args)
            .env("BUZZX_CONFIG", "/nonexistent/buzzx-test-config")
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
            {"channel_id": unnamed, "name": unnamed},
            {"channel_id": named, "name": "buzzx-cli"},
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
fn a_reply_to_an_unresolved_event_is_not_found_and_writes_nothing() {
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
    assert_eq!(json["error"], json!("not_found"));
    assert_eq!(json["event_id"], json!(missing));
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
    let relay = FakeRelay::start();
    let keys = keys();
    let cases: [(&[&str], Option<&str>, &str); 5] = [
        (
            &[
                "messages",
                "get",
                "--channel",
                CHANNEL,
                "--event",
                &"cd".repeat(32),
            ],
            None,
            "invalid_input",
        ),
        (&["messages", "get"], None, "invalid_input"),
        (
            &["messages", "send", "--channel", CHANNEL],
            None,
            "invalid_input",
        ),
        (
            &["messages", "send", "--channel", "nope", "--content", "hi"],
            None,
            "invalid_input",
        ),
        (
            &["messages", "send", "--channel", CHANNEL, "--content", "-"],
            Some(""),
            "invalid_input",
        ),
    ];
    for (args, stdin, category) in cases {
        let (code, json, stderr) = run(&relay.url, &keys, args, stdin);
        assert_eq!(code, 1, "{args:?} stderr: {stderr}");
        assert_eq!(json["error"], json!(category), "{args:?}: {json}");
        assert!(json["message"].is_string(), "{args:?}: {json}");
    }
    assert!(
        relay.writes().is_empty(),
        "bad input never reaches the relay"
    );
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
